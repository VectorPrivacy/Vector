//! The video half of `CallOverlay.svelte`: their pictures with ours in a corner, one of theirs
//! filling the window, and each of our pictures' quality controls in the panel.

use std::time::Instant;

use gpui::{AnyElement, InteractiveElement, IntoElement, ObjectFit, ParentElement, StatefulInteractiveElement, Styled, StyledImage, div, prelude::FluentBuilder, px, svg};
use crate::pic::img;

use crate::call::{Act, Call, OnAct, Quality, Seen, call_btn, dot, label, toggle};
use crate::data::Store;
use crate::theme::{ACCENT, c, ca, white_a};

const QUALITY: [&[(&str, u32)]; 2] = [
    &[("Low", 1), ("Medium", 3), ("High", 5), ("Best", 6)],
    &[("Low", 1), ("Medium", 3), ("High", 5), ("Best", 7), ("Ultra", 8)],
];
const FPS: [&[u32]; 2] = [&[15, 30], &[5, 10, 15, 30, 60]];
const BLOCK_TITLE: [&str; 2] = ["Camera quality", "Screenshare quality"];
const KIND: [&str; 2] = ["camera", "screen"];

pub fn showing(call: &Call) -> bool {
    call.live() && call.video_mine.iter().chain(call.video_peer.iter()).any(|&b| b)
}

/// Of their pictures the screen leads and the camera sits inset; alone, either leads.
fn lead(call: &Call) -> Option<usize> {
    if call.video_peer[1] {
        Some(1)
    } else if call.video_peer[0] {
        Some(0)
    } else {
        None
    }
}

pub fn max_open(call: &Call) -> Option<usize> {
    call.maximized.filter(|&k| call.live() && call.video_peer[k])
}

fn picture(seen: &Seen, k: usize, fit: ObjectFit) -> Option<AnyElement> {
    seen.peer[k].as_ref().map(|(image, _, _)| img(image.clone()).absolute().inset_0().size_full().object_fit(fit).into_any_element())
}

fn video_btn(id: &'static str, k: usize, call: &Call, enabled: bool, on: &OnAct) -> impl IntoElement {
    let state = call.video_mine[k];
    let o = on.clone();
    call_btn(id, state)
        .tooltip(crate::kit::tooltip(match (k, state) {
            (0, true) => "Turn the camera off",
            (0, false) => "Turn the camera on",
            (_, true) => "Stop sharing the screen",
            (_, false) => "Share the screen",
        }))
        .when(!enabled, |d| d.opacity(0.35).cursor_default())
        .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| cx.stop_propagation())
        .when(enabled, |d| d.on_click(move |_, w, cx| o(Act::Video(k, !state), w, cx)))
        .child(if k == 0 {
            svg().path("icons/video.svg").size(px(18.7)).text_color(if state { c(ACCENT) } else { white_a(0.88) })
        } else {
            svg().path("ui/call-screen.svg").size(px(20.)).text_color(if state { c(ACCENT) } else { white_a(0.88) })
        })
}

/// The camera and screen buttons, in the pill's row while the call can carry video.
pub fn buttons(call: &Call, seen: &Seen, on: &OnAct) -> Vec<AnyElement> {
    if !call.live() || !seen.can_send {
        return Vec::new();
    }
    vec![
        video_btn("call-camera", 0, call, true, on).into_any_element(),
        video_btn("call-screen", 1, call, true, on).into_any_element(),
    ]
}

fn small_btn(id: &'static str, icon: &'static str) -> gpui::Stateful<gpui::Div> {
    div()
        .id(id)
        .absolute()
        .size(px(34.))
        .rounded_full()
        .flex()
        .items_center()
        .justify_center()
        .bg(ca(0, 0, 0, 0.55))
        .text_color(white_a(0.88))
        .hover(|d| d.bg(white_a(0.14)).text_color(c(0xffffff)))
        .cursor_pointer()
        .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| cx.stop_propagation())
        .child(svg().path(icon).size(px(20.)).text_color(white_a(0.88)))
}

fn mine(call: &Call, seen: &Seen, k: usize) -> AnyElement {
    let (w, h) = if k == 1 { (112., 70.75) } else { (112., 84.5) };
    let unseen = call.paused_by_peer;
    div()
        .relative()
        .w(px(w))
        .h(px(h))
        .flex_none()
        .rounded(px(8.))
        .border_1()
        .border_color(c(0x444444))
        .bg(c(0x111111))
        .overflow_hidden()
        .children(seen.mine[k].as_ref().map(|image| {
            img(image.clone())
                .size_full()
                .object_fit(if k == 1 { ObjectFit::Contain } else { ObjectFit::Cover })
                .grayscale(unseen)
                .opacity(if unseen { 0.45 } else { 1. })
        }))
        .when(unseen, |d| {
            d.child(svg().path("icons/eye-off.svg").absolute().top(px(6.)).right(px(6.)).size(px(18.)).text_color(white_a(0.9)))
        })
        .into_any_element()
}

/// Their pictures above the panel, ours in the bottom corner.
pub fn pictures(call: &Call, store: &Store, seen: &Seen, w: f32, max_h: f32, on: &OnAct) -> AnyElement {
    let lead = lead(call);
    let inset = (call.video_peer[0] && call.video_peer[1]).then_some(0);
    let aspect = lead
        .and_then(|k| seen.peer[k].as_ref())
        .map(|(_, pw, ph)| *pw as f32 / (*ph).max(1) as f32)
        .unwrap_or(16. / 9.);
    let h = (w / aspect).min(max_h).round();
    let idle = |d: gpui::Div| {
        d.absolute()
            .inset_0()
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .gap(px(10.))
            .text_size(px(13.))
            .text_color(c(0xb2b2b2))
    };
    let name = store.name(&call.peer);
    div()
        .id("call-pictures")
        .relative()
        .w(px(w))
        .h(px(h))
        .flex_none()
        .bg(c(0x000000))
        .overflow_hidden()
        .cursor_default()
        .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| cx.stop_propagation())
        .map(|d| match lead {
            Some(k) => {
                let o = on.clone();
                d.children(picture(seen, k, ObjectFit::Contain))
                    .when(seen.peer[k].is_none(), |d| d.child(idle(div()).child(format!("Waiting for their {}…", KIND[k]))))
                    .child(small_btn("call-picture-max", "ui/call-max.svg").tooltip(crate::kit::tooltip("Fill the window")).top(px(8.)).right(px(8.)).on_click(move |_, w, cx| o(Act::Maximize(Some(k)), w, cx)))
                    .when(k == 1, |d| {
                        d.child(
                            div()
                                .absolute()
                                .top(px(8.))
                                .left(px(8.))
                                .px(px(8.))
                                .py(px(3.))
                                .rounded_full()
                                .bg(ca(0, 0, 0, 0.55))
                                .text_size(px(11.))
                                .line_height(px(24.))
                                .text_color(c(0xdddddd))
                                .child(format!("{name} is sharing a screen")),
                        )
                    })
            }
            None => d.child(idle(div()).child(crate::ui::avatar(store, &call.peer, 64.)).child("Their camera is off")),
        })
        .children(inset.map(|k| {
            let iw = (w * 0.26).min(150.);
            let o = on.clone();
            div()
                .id("call-picture-inset")
                .tooltip(crate::kit::tooltip("Fill the window with their camera"))
                .absolute()
                .bottom(px(8.))
                .left(px(8.))
                .w(px(iw))
                .h(px(((iw - 2.) * 0.75 + 2.).round()))
                .rounded(px(8.))
                .border_1()
                .border_color(c(0x444444))
                .bg(c(0x111111))
                .overflow_hidden()
                .cursor_pointer()
                .on_click(move |_, w, cx| o(Act::Maximize(Some(k)), w, cx))
                .children(picture(seen, k, ObjectFit::Cover))
        }))
        .child(
            div()
                .absolute()
                .bottom(px(8.))
                .right(px(8.))
                .flex()
                .flex_row_reverse()
                .gap(px(6.))
                .when(call.video_mine[1], |d| d.child(mine(call, seen, 1)))
                .when(call.video_mine[0], |d| d.child(mine(call, seen, 0))),
        )
        .into_any_element()
}

/// One of their pictures over everything; their other one rides along the bottom.
pub fn maximized(call: &Call, seen: &Seen, chrome: f32, mic: AnyElement, hangup: AnyElement, on: &OnAct) -> Option<AnyElement> {
    let k = max_open(call)?;
    let other = 1 - k;
    let other = call.video_peer[other].then_some(other);
    let controls = call.max_controls.is_some_and(|t| t > Instant::now());
    let quality = call.quality().filter(|q| matches!(q, Quality::Fair | Quality::Poor));
    let (wake, back, swap) = (on.clone(), on.clone(), on.clone());
    let restore = on.clone();
    let strip = div()
        .absolute()
        .bottom(px(16.))
        .left_0()
        .right_0()
        .flex()
        .justify_center()
        .child(
            div()
                .id("call-max-strip")
                .flex()
                .gap(px(10.))
                .px(px(12.))
                .py(px(8.))
                .rounded_full()
                .bg(ca(20, 20, 20, 0.85))
                .border_1()
                .border_color(c(0x333333))
                .when(!controls, |d| d.opacity(0.))
                .child(video_btn("call-max-camera", 0, call, seen.can_send, on))
                .child(video_btn("call-max-screen", 1, call, seen.can_send, on))
                .child(mic)
                .child(hangup),
        );
    Some(
        div()
            .id("call-max")
            .absolute()
            .top(px(chrome))
            .left_0()
            .right_0()
            .bottom_0()
            .bg(c(0x000000))
            .occlude()
            .on_mouse_move(move |_, w, cx| wake(Act::WakeControls, w, cx))
            .on_click(move |e, w, cx| {
                if e.click_count() == 2 {
                    back(Act::Maximize(None), w, cx);
                }
            })
            .children(picture(seen, k, ObjectFit::Contain))
            .children(quality.map(|q| {
                div()
                    .absolute()
                    .top(px(12.))
                    .left(px(12.))
                    .flex()
                    .items_center()
                    .gap(px(6.))
                    .px(px(10.))
                    .py(px(4.))
                    .rounded_full()
                    .bg(ca(0, 0, 0, 0.6))
                    .text_size(px(12.))
                    .text_color(c(0xdddddd))
                    .child(dot(q))
                    .child(q.label())
            }))
            .child(
                div()
                    .id("call-max-restore")
                    .tooltip(crate::kit::tooltip("Back to the call window (Esc)"))
                    .absolute()
                    .top(px(12.))
                    .right(px(12.))
                    .size(px(32.))
                    .p(px(6.))
                    .rounded(px(8.))
                    .bg(ca(0, 0, 0, 0.85))
                    .cursor_pointer()
                    .when(!controls, |d| d.opacity(0.))
                    .on_click(move |_, w, cx| restore(Act::Maximize(None), w, cx))
                    .child(svg().path("ui/call-restore.svg").size_full().text_color(c(0xffffff))),
            )
            .children(other.map(|o| {
                div().absolute().bottom(px(if controls { 72. } else { 16. })).left_0().right_0().flex().justify_center().child(
                    div()
                        .id("call-max-other")
                        .tooltip(crate::kit::tooltip("Swap"))
                        .w(px(320.))
                        .h(px(201.))
                        .rounded(px(10.))
                        .border_1()
                        .border_color(c(0x555555))
                        .bg(c(0x111111))
                        .overflow_hidden()
                        .cursor_pointer()
                        .on_click(move |_, w, cx| swap(Act::Maximize(Some(o)), w, cx))
                        .children(picture(seen, o, ObjectFit::Contain)),
                )
            }))
            .child(strip)
            .into_any_element(),
    )
}

fn compact_select(id: (&'static str, usize), label_text: String, open: bool, choices: Vec<(String, Option<u32>)>, field: (usize, bool), on: &OnAct) -> AnyElement {
    let (o, p) = (on.clone(), on.clone());
    div()
        .relative()
        .child(
            div()
                .id(gpui::ElementId::NamedInteger(id.0.into(), id.1 as u64))
                .flex()
                .items_center()
                .justify_between()
                .gap(px(6.))
                .h(px(22.))
                .min_w(px(96.))
                .px(px(6.))
                .py(px(3.))
                .rounded(px(6.))
                .border_1()
                .border_color(c(0x333333))
                .bg(c(0x262626))
                .text_size(px(12.))
                .line_height(px(14.4))
                .text_color(c(0xffffff))
                .cursor_pointer()
                .on_click(move |_, w, cx| o(Act::PrefOpen(if open { None } else { Some(field) }), w, cx))
                .child(label_text)
                .child(svg().path("ui/caret-down.svg").w(px(8.)).h(px(5.)).text_color(c(0xaaaaaa))),
        )
        .when(open, |d| {
            d.child(
                gpui::deferred(
                    div()
                        .absolute()
                        .top(px(24.))
                        .left_0()
                        .min_w(px(96.))
                        .py(px(4.))
                        .rounded(px(6.))
                        .border_1()
                        .border_color(c(0x333333))
                        .bg(c(0x262626))
                        .occlude()
                        .children(choices.into_iter().enumerate().map(|(i, (text, v))| {
                            let p = p.clone();
                            div()
                                .id(i)
                                .px(px(8.))
                                .py(px(4.))
                                .text_size(px(12.))
                                .text_color(c(0xffffff))
                                .cursor_pointer()
                                .hover(|d| d.bg(c(0x333333)))
                                .on_click(move |_, w, cx| p(Act::Pref(field.0, field.1, v), w, cx))
                                .child(text)
                        })),
                )
                .with_priority(20),
            )
        })
        .into_any_element()
}

/// Our pictures' quality blocks in the panel, each while it is on.
pub fn panel_blocks(call: &Call, on: &OnAct) -> Vec<AnyElement> {
    (0..2)
        .filter(|&k| call.live() && call.video_mine[k])
        .map(|k| {
            let note = call
                .video_stats(k)
                .filter(|s| s.0 > 0)
                .map(|(w, h, fps, kbps)| format!("{w}×{h} · {fps} fps · {kbps} kbit/s"))
                .unwrap_or_default();
            let (rung, fps) = call.prefs[k];
            let mut q: Vec<(String, Option<u32>)> = vec![("Adaptive".into(), None)];
            q.extend(QUALITY[k].iter().map(|(l, r)| (l.to_string(), Some(*r))));
            let q_label = q.iter().find(|(_, v)| *v == rung).map(|(l, _)| l.clone()).unwrap_or_else(|| "Adaptive".into());
            let mut f: Vec<(String, Option<u32>)> = vec![("Adaptive".into(), None)];
            f.extend(FPS[k].iter().map(|r| (format!("{r} fps"), Some(*r))));
            let f_label = f.iter().find(|(_, v)| *v == fps).map(|(l, _)| l.clone()).unwrap_or_else(|| "Adaptive".into());
            let row = |l: &'static str, v: AnyElement| div().flex().items_center().child(label(l)).child(v);
            div()
                .flex()
                .flex_col()
                .gap(px(10.))
                .child(
                    div()
                        .mt(px(6.))
                        .pt(px(10.))
                        .border_t_1()
                        .border_color(c(0x262626))
                        .flex()
                        .items_baseline()
                        .justify_between()
                        .gap(px(8.))
                        .text_size(px(11.))
                        .child(div().font_weight(gpui::FontWeight::SEMIBOLD).text_color(c(ACCENT)).child(BLOCK_TITLE[k].to_uppercase()))
                        .child(div().truncate().text_color(c(0x8a8a8a)).child(note)),
                )
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .gap(px(6.))
                        .child(row("Quality", compact_select(("call-q", k), q_label, call.pref_open == Some((k, false)), q, (k, false), on)))
                        .child(row("Frame rate", compact_select(("call-f", k), f_label, call.pref_open == Some((k, true)), f, (k, true), on)))
                        .when(k == 1, |d| {
                            let change = on.clone();
                            d.child(row(
                                "Window",
                                div()
                                    .id("call-change-screen")
                                    .px(px(10.))
                                    .py(px(3.))
                                    .rounded(px(6.))
                                    .border_1()
                                    .border_color(c(0x333333))
                                    .bg(c(0x262626))
                                    .hover(|d| d.bg(c(0x333333)))
                                    .text_size(px(12.))
                                    .text_color(c(0xffffff))
                                    .cursor_pointer()
                                    .on_click(move |_, w, cx| change(Act::ChangeScreen, w, cx))
                                    .child("Change…")
                                    .into_any_element(),
                            ))
                            .child(row(
                                "Sound",
                                if call.share_audio_native {
                                    div()
                                        .flex()
                                        .flex_1()
                                        .items_center()
                                        .justify_between()
                                        .gap(px(8.))
                                        .text_size(px(12.5))
                                        .text_color(c(0xf6f6f6))
                                        .child(div().flex().items_center().gap(px(8.)).child(if call.share_audio_mine { "Sharing" } else { "Not shared" }).when(
                                            call.share_audio_denied && !call.share_audio_mine,
                                            |d| {
                                                d.child(
                                                    div()
                                                        .px(px(7.))
                                                        .py(px(1.))
                                                        .rounded_full()
                                                        .border_1()
                                                        .border_color(ca(255, 196, 0, 0.45))
                                                        .bg(ca(255, 196, 0, 0.12))
                                                        .text_color(c(0xffc400))
                                                        .text_size(px(10.5))
                                                        .font_weight(gpui::FontWeight::SEMIBOLD)
                                                        .child("System permission required"),
                                                )
                                            },
                                        ))
                                        .child(toggle("call-share-audio", call.share_audio_mine, Act::ShareAudio(!call.share_audio_mine), on))
                                        .into_any_element()
                                } else {
                                    div().truncate().text_size(px(12.)).text_color(c(0xb2b2b2)).child("Not offered by this window").into_any_element()
                                },
                            ))
                        }),
                )
                .into_any_element()
        })
        .collect()
}

/// The Connection grid's lines for what they send.
pub fn peer_lines(call: &Call, seen: &Seen) -> Vec<AnyElement> {
    ["Their camera", "Their screen"]
        .into_iter()
        .enumerate()
        .filter_map(|(k, l)| {
            let (_, w, h) = seen.peer[k].as_ref().filter(|_| call.video_peer[k])?;
            Some(
                div()
                    .flex()
                    .items_center()
                    .child(label(l))
                    .child(div().truncate().text_size(px(12.)).text_color(c(0xb2b2b2)).child(format!("{w}×{h} · {} fps", seen.dec_fps[k])))
                    .into_any_element(),
            )
        })
        .collect()
}
