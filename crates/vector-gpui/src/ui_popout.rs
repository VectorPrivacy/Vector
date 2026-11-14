//! The floating player on the app: what leaves a chat playing, and the box itself.

use std::rc::Rc;

use gpui::{AnyElement, Bounds, Context, CursorStyle, HitboxBehavior, MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, ObjectFit, Pixels, SharedString, Window, canvas, div, prelude::*, px, svg};
use crate::pic::img;

use crate::popout::{Act, Corner, Item, Kind};
use crate::theme::*;
use super::VectorApp;

const TITLE: u32 = 0xebebeb;

type On = Rc<dyn Fn(Act, &mut Window, &mut gpui::App)>;

impl VectorApp {
    fn win(window: &Window) -> (f32, f32) {
        let s = window.viewport_size();
        (f32::from(s.width), f32::from(s.height))
    }

    /// Leaving the open chat for `next`: what plays floats (or stops, with the player off), and
    /// coming back to where the floating media came from takes it back into its row.
    pub(crate) fn leave_media(&mut self, next: &str, window: &Window) {
        if self.popout.item.as_ref().is_some_and(|i| i.chat == next) {
            self.popout.close();
        }
        let Some(prev) = self.open.clone().map(String::from) else { return };
        let mut media = self.media.borrow_mut();
        let audio = media.audio.playing().map(String::from);
        let video = media.videos.playing().map(String::from);
        let held = self.popout.item.as_ref().map(|i| i.att.clone());
        if self.popout.enabled {
            let in_prev = |att: &str| -> Option<String> {
                let store = self.store.borrow();
                store
                    .chat(&prev)?
                    .messages
                    .iter()
                    .find(|m| m.attachments.iter().any(|a| a.id == att))
                    .map(|m| m.id.clone())
            };
            let music = audio.as_deref().filter(|a| held.as_deref() != Some(*a)).and_then(|a| {
                in_prev(a).map(|msg| Item { kind: Kind::Audio, att: a.to_string(), msg, chat: prev.clone() })
            });
            let floating_music = held.is_some() && audio.is_some() && audio == held;
            let clip = video
                .as_deref()
                .filter(|_| music.is_none() && !floating_music)
                .and_then(|v| in_prev(v).map(|msg| Item { kind: Kind::Video, att: v.to_string(), msg, chat: prev.clone() }));
            if let Some(item) = music.or(clip) {
                if let Some(old) = self.popout.item.as_ref()
                    && old.kind == Kind::Audio
                    && old.att != item.att
                {
                    media.audio.pause(&old.att);
                }
                self.popout.open(item, Self::win(window));
            }
        } else if let Some(a) = audio {
            media.audio.pause(&a);
        }
        let keep = self.popout.item.as_ref().filter(|i| i.kind == Kind::Video).map(|i| i.att.clone());
        media.videos.stop_except(keep.as_deref());
    }

    /// Media starting in the open chat takes the floating player's place when it is the same kind.
    pub(crate) fn yield_popout(&mut self, kind: Kind, att: &str) {
        let Some(i) = self.popout.item.as_ref() else { return };
        if i.kind != kind || i.att == att {
            return;
        }
        if let Some(i) = self.popout.close() {
            let mut media = self.media.borrow_mut();
            match i.kind {
                Kind::Audio => media.audio.pause(&i.att),
                Kind::Video => media.videos.stop(&i.att),
            }
        }
    }

    pub(crate) fn on_popout(&mut self, act: Act, window: &mut Window, cx: &mut Context<Self>) {
        let Some(item) = self.popout.item.clone() else { return };
        let aspect = self.media.borrow().videos.clip(&item.att).map_or(16. / 9., |c| c.aspect());
        match act {
            Act::Toggle => match item.kind {
                Kind::Audio => self.audio_act(item.msg, item.att, crate::audio::Act::Toggle, cx),
                Kind::Video => self.video_act(item.msg, item.att, crate::video::Act::Toggle, cx),
            },
            Act::Seek(f, done) => {
                self.popout.scrub = (!done).then_some(f);
                match item.kind {
                    Kind::Audio => self.audio_act(item.msg, item.att, crate::audio::Act::Seek(f, done), cx),
                    Kind::Video => self.video_act(item.msg, item.att, crate::video::Act::Seek(f, done), cx),
                }
            }
            Act::Songs => self.popout.tracks_open = !self.popout.tracks_open,
            Act::Art => self.view_art(&item.att, window, cx),
            Act::Lyrics => self.audio_act(item.msg, item.att, crate::audio::Act::Lyrics, cx),
            Act::Transcribe => self.audio_act(item.msg, item.att, crate::audio::Act::Transcribe, cx),
            Act::Line(l) => self.audio_act(item.msg, item.att, crate::audio::Act::Line(l), cx),
            Act::SeekTo(ms) => self.audio_act(item.msg, item.att, crate::audio::Act::SeekTo(ms), cx),
            Act::Track(i) => {
                self.audio_act(item.msg, item.att, crate::audio::Act::Album(crate::audio::AlbumAct::Track(i)), cx)
            }
            Act::Prev | Act::Next => {
                let step = if matches!(act, Act::Prev) { crate::audio::AlbumAct::Prev } else { crate::audio::AlbumAct::Next };
                self.audio_act(item.msg, item.att, crate::audio::Act::Album(step), cx);
            }
            Act::Video(crate::video::Act::Toggle) if self.popout.swallow() => {}
            Act::Video(a) => self.video_act(item.msg, item.att, a, cx),
            Act::Close => {
                self.popout.close();
                let mut media = self.media.borrow_mut();
                match item.kind {
                    Kind::Audio => media.audio.pause(&item.att),
                    Kind::Video => media.videos.stop(&item.att),
                }
            }
            Act::ShowInChat => {
                self.open_chat(SharedString::from(item.chat.clone()), window, cx);
                self.jump_to_message(item.msg, cx);
            }
            Act::SoundOnly => {
                self.popout.toggle_sound();
                let w = self.popout.width(Self::win(window), aspect);
                let h = self.popout.height(w, aspect);
                self.popout.clamp(Self::win(window), (w, h));
            }
            a => {
                if !self.popout.gesture(&a, Self::win(window), aspect) && !matches!(a, Act::Press(..) | Act::Release) {
                    return;
                }
            }
        }
        cx.notify();
    }

    /// `.media-popout`: anchored by its bottom-right corner.
    pub(crate) fn popout_view(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Option<AnyElement> {
        let item = self.popout.item.clone()?;
        let win = Self::win(window);
        let media = self.media.borrow();
        // A clip that played to its end leaves on its own.
        if item.kind == Kind::Video
            && let Some(c) = media.videos.clip(&item.att)
            && !c.playing
            && !c.loading
            && c.duration() > 0
            && c.position() >= c.duration()
        {
            drop(media);
            self.popout.close();
            return None;
        }
        let clip = media.videos.clip(&item.att);
        let aspect = clip.map_or(16. / 9., |c| c.aspect());
        self.popout.grew(win);
        let width = self.popout.width(win, aspect);
        let height = self.popout.height(width, aspect);
        let (w0, h0) = (width, height);
        self.popout.clamp(win, (w0, h0));
        let sound_only = self.popout.sound_only();
        let picture = item.kind == Kind::Video && !sound_only;
        let this = cx.entity().downgrade();
        let on: On = Rc::new(move |a, w, cx| {
            this.update(cx, |app, cx| app.on_popout(a, w, cx)).ok();
        });

        let store = self.store.borrow();
        let msg = store.chat(&item.chat).and_then(|c| c.messages.iter().find(|m| m.id == item.msg));
        let att = msg.and_then(|m| m.attachments.iter().find(|a| a.id == item.att));
        let voice = item.kind == Kind::Audio && att.is_some_and(|a| a.name.is_empty());
        let transcribable = voice && att.is_some_and(|a| crate::transcript::supported(a, true, false));
        let chat_name = match store.community_of(&item.chat) {
            Some(c) => {
                let ch = c.channel(&item.chat).map(|ch| ch.name.clone()).unwrap_or_default();
                format!("{} \u{203a} {ch}", c.name)
            }
            None => store.name(&item.chat),
        };
        let tags = media.audio.tags(&item.att).cloned();
        let who = msg.map(|m| {
            if m.mine { store.my_npub.clone() } else { m.npub.clone().unwrap_or_else(|| item.chat.clone()) }
        });
        let player = media.audio.player(&item.att).filter(|_| item.kind == Kind::Audio);
        let album = player.and_then(|p| p.album.as_ref()).filter(|a| a.tracks.len() > 1);
        let (title, sub) = if let Some(a) = album {
            let i = a.track.max(0) as usize;
            let name = crate::audio::split_title(&a.tracks[i].title).0;
            let from = tags
                .as_ref()
                .and_then(|t| t.album.as_deref().map(|al| crate::audio::split_title(al).0).or_else(|| t.artist.clone()));
            let sub = [Some(format!("{} of {}", i + 1, a.tracks.len())), from].into_iter().flatten().collect::<Vec<_>>().join(" \u{b7} ");
            (name, sub)
        } else if voice {
            let name = who.as_deref().map(|w| store.chat_name(w, &item.chat)).unwrap_or_default();
            let sub = if chat_name != name { chat_name.clone() } else { "Voice Message".into() };
            (name, sub)
        } else if let Some(artist) = tags.as_ref().and_then(|t| t.artist.clone()) {
            (tags.as_ref().and_then(|t| t.title.clone()).unwrap_or_else(|| att.map_or(String::new(), |a| a.name.clone())), artist)
        } else {
            let fallback = if item.kind == Kind::Video { "Video" } else { "Audio" };
            let name = tags
                .as_ref()
                .and_then(|t| t.title.clone())
                .or_else(|| att.map(|a| a.name.clone()).filter(|n| !n.is_empty()))
                .unwrap_or_else(|| fallback.into());
            (name, chat_name.clone())
        };
        let accent = tags.as_ref().and_then(|t| t.accent).unwrap_or(c(PRIMARY));
        let glow = if item.kind == Kind::Audio { crate::audio::glows(tags.as_deref(), 0.) } else { Vec::new() };

        let thumb: AnyElement = match item.kind {
            Kind::Video => div()
                .size(px(44.))
                .rounded(px(8.))
                .overflow_hidden()
                .bg(c(0x262626))
                .children(clip.and_then(|c| c.picture()).map(|p| img(p).size_full().object_fit(ObjectFit::Cover)))
                .into_any_element(),
            Kind::Audio if voice => match who.as_deref() {
                Some(w) => crate::ui::avatar(&store, w, 44.),
                None => div().size(px(44.)).into_any_element(),
            },
            Kind::Audio => match tags.as_ref().and_then(|t| t.cover.clone()) {
                Some(art) => div()
                    .id("popout-art")
                    .size(px(44.))
                    .rounded(px(8.))
                    .overflow_hidden()
                    .cursor(CursorStyle::PointingHand)
                    .on_click({
                        let on = on.clone();
                        move |_, w, cx| {
                            cx.stop_propagation();
                            on(Act::Art, w, cx)
                        }
                    })
                    .child(img(art).size_full().object_fit(ObjectFit::Cover))
                    .into_any_element(),
                None => div()
                    .size(px(44.))
                    .rounded(px(8.))
                    .bg(c(0x262626))
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(svg().path("icons/volume-max.svg").size(px(22.)).text_color(white_a(0.7)))
                    .into_any_element(),
            },
        };
        drop(store);

        let compact = width < 340.;
        let lyrics_open = media.audio.sheets.get(&item.att).is_some_and(|s| s.open);
        let has_lyrics = tags.as_ref().is_some_and(|t| t.lyrics.is_some());
        let transcript = media.audio.transcripts.get(&item.att);
        let transcript_icon = match transcript.map(|t| (&t.phase, t.open)) {
            Some((crate::transcript::Phase::Loading, _)) => "icons/loading.svg",
            Some((crate::transcript::Phase::Ready { .. }, true)) => "icons/file-minus.svg",
            _ => "icons/file-plus.svg",
        };
        let transcript_tip = match transcript.map(|t| (&t.phase, t.open)) {
            Some((crate::transcript::Phase::Ready { .. }, true)) => "Hide transcript",
            Some((crate::transcript::Phase::Ready { .. }, false)) => "Show transcript",
            _ => "Transcribe",
        };
        let transcript_lit = transcript.is_some_and(|t| t.open && matches!(t.phase, crate::transcript::Phase::Ready { .. }));
        let action = |key: &'static str, icon: &'static str, tip: &'static str, act: Act, lit: bool| {
            let on = on.clone();
            div()
                .id(key)
                .tooltip(crate::kit::tooltip(tip))
                .size(px(32.))
                .flex_none()
                .rounded(px(8.))
                .flex()
                .items_center()
                .justify_center()
                .cursor_pointer()
                .hover(|s| s.bg(white_a(0.07)))
                .on_click(move |_, w, cx| {
                    cx.stop_propagation();
                    on(act.clone(), w, cx)
                })
                .child(svg().path(icon).size(px(20.)).text_color(if lit { accent } else { white_a(0.7) }))
        };
        let is_album_now = album.is_some();
        let actions = div()
            .flex()
            .gap(px(4.))
            .flex_none()
            .when(is_album_now && !compact, |d| {
                d.child(action("popout-songs", "icons/list-music.svg", if self.popout.tracks_open { "Hide songs" } else { "Songs" }, Act::Songs, self.popout.tracks_open))
            })
            .when(has_lyrics && !compact, |d| d.child(action("popout-lyrics", "icons/align-left.svg", if lyrics_open { "Hide lyrics" } else { "Lyrics" }, Act::Lyrics, lyrics_open)))
            .when(transcribable, |d| d.child(action("popout-transcribe", transcript_icon, transcript_tip, Act::Transcribe, transcript_lit)))
            .when(item.kind == Kind::Video, |d| {
                d.child(action(
                    "popout-sound",
                    if sound_only { "icons/video.svg" } else { "icons/volume-max.svg" },
                    if sound_only { "Show video" } else { "Sound only" },
                    Act::SoundOnly,
                    false,
                ))
            })
            .child(action("popout-chat", "icons/chat-bubble.svg", "Show in chat", Act::ShowInChat, false))
            .child(action("popout-close", "icons/x.svg", "Close", Act::Close, false));
        let words = div()
            .flex_1()
            .min_w_0()
            .flex()
            .flex_col()
            .gap(px(2.))
            .child(
                div()
                    .text_size(px(14.72))
                    .line_height(px(18.4))
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .text_color(white_a(0.92))
                    .truncate()
                    .child(title),
            )
            .child(div().text_size(px(12.48)).line_height(px(15.6)).text_color(white_a(0.55)).truncate().child(sub));

        let (playing, loading, pos, duration, bars) = match item.kind {
            Kind::Audio => {
                let p = media.audio.player(&item.att);
                (
                    p.is_some_and(|p| p.playing),
                    p.is_some_and(|p| p.loading),
                    p.map_or(0, |p| p.position()),
                    p.map_or(0, |p| p.duration).max(media.audio.duration(&item.att)),
                    p.map(|p| p.display().to_vec()).unwrap_or_default(),
                )
            }
            Kind::Video => (
                clip.is_some_and(|c| c.playing),
                clip.is_some_and(|c| c.loading),
                clip.map_or(0, |c| c.position()),
                clip.map_or(0, |c| c.duration()),
                Vec::new(),
            ),
        };
        let (start, end) = album.map_or((0, duration), |a| a.now(duration));
        let span = end.saturating_sub(start);
        let pos = match self.popout.scrub {
            Some(f) if span > 0 => start + (f * span as f32) as u64,
            _ => pos,
        };
        let into = pos.saturating_sub(start).min(span.max(pos.saturating_sub(start)));
        let frac = if span > 0 { (into as f32 / span as f32).clamp(0., 1.) } else { 0. };
        let time = format!("{} / {}", crate::audio::format_time(into), crate::audio::format_time(span));
        let is_album = album.is_some();
        let scrubbing = self.popout.scrub.is_some();

        let mut panels: Vec<AnyElement> = Vec::new();
        if let Some(a) = album.filter(|_| self.popout.tracks_open && !compact) {
            let rows = a.tracks.iter().enumerate().map(|(i, t)| {
                let on = on.clone();
                let (main, note) = crate::audio::split_title(&t.title);
                let len = t.end.unwrap_or(duration).saturating_sub(t.start);
                let cur = i as isize == a.track;
                div()
                    .id(SharedString::from(format!("popout-track-{i}")))
                    .flex()
                    .items_center()
                    .gap(px(10.))
                    .px(px(4.))
                    .py(px(6.))
                    .rounded(px(8.))
                    .text_size(px(13.12))
                    .line_height(px(18.37))
                    .text_color(if cur { accent } else { white_a(0.92) })
                    .when(cur, |d| d.font_weight(gpui::FontWeight::SEMIBOLD))
                    .cursor_pointer()
                    .hover(|s| s.bg(white_a(0.05)))
                    .on_click(move |_, w, cx| {
                        cx.stop_propagation();
                        on(Act::Track(i), w, cx)
                    })
                    .child(
                        div()
                            .w(px(26.))
                            .flex_none()
                            .flex()
                            .justify_center()
                            .font_weight(gpui::FontWeight::NORMAL)
                            .text_color(white_a(0.34))
                            .child((i + 1).to_string()),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .flex()
                            .overflow_hidden()
                            .whitespace_nowrap()
                            .text_ellipsis()
                            .child(main)
                            .when(!note.is_empty(), |d| {
                                d.child(div().ml(px(4.)).font_weight(gpui::FontWeight::NORMAL).opacity(0.5).child(note))
                            }),
                    )
                    .child(
                        div()
                            .flex_none()
                            .font_weight(gpui::FontWeight::NORMAL)
                            .text_color(white_a(0.34))
                            .child(crate::audio::format_time(len)),
                    )
            });
            panels.push(
                div()
                    .id("popout-tracks")
                    .mt(px(10.))
                    .max_h(px(200.))
                    .overflow_y_scroll()
                    .children(rows)
                    .into_any_element(),
            );
        }
        if lyrics_open
            && !compact
            && let (Some(l), Some(sheet)) = (tags.as_ref().and_then(|t| t.lyrics.as_ref()), media.audio.sheets.get(&item.att))
        {
            let on = on.clone();
            if let Some(v) = crate::lyrics::view(
                &format!("pop-{}", item.att),
                l,
                sheet,
                pos,
                playing,
                Rc::new(move |line, w, cx| on(Act::Line(line), w, cx)),
            ) {
                panels.push(div().mt(px(10.)).h(px(200.)).overflow_hidden().child(v).into_any_element());
            }
        }
        if let Some(t) = transcript {
            let on = on.clone();
            if let Some(v) = crate::transcript::view(
                &format!("pop-{}", item.att),
                t,
                pos,
                playing,
                accent,
                Rc::new(move |ms, w, cx| on(Act::SeekTo(ms), w, cx)),
            ) {
                panels.push(
                    div()
                        .id("popout-transcript")
                        .mt(px(10.))
                        .max_h(px(180.))
                        .overflow_y_scroll()
                        .text_size(px(14.4))
                        .child(v)
                        .into_any_element(),
                );
            }
        }
        let body: AnyElement = if picture {
            let pic_w = width - 2.;
            let pic_h = pic_w / aspect;
            let video = clip.map(|c| {
                let on = on.clone();
                crate::video::view(
                    &format!("pop-{}", item.att),
                    c,
                    false,
                    Some((pic_w, pic_h)),
                    0.,
                    Rc::new(move |a, w, cx| on(Act::Video(a), w, cx)),
                )
            });
            div()
                .flex()
                .flex_col()
                .pb(px(10.))
                .gap(px(8.))
                .child(div().w(px(pic_w)).h(px(pic_h)).bg(gpui::black()).children(video))
                .child(
                    div()
                        .flex()
                        .items_start()
                        .gap(px(12.))
                        .pl(px(12.))
                        .pr(px(8.))
                        .child(words.mt(px(-2.)))
                        .child(actions),
                )
                .into_any_element()
        } else {
            let on_t = on.clone();
            let icon = if loading {
                "icons/loading.svg"
            } else if playing {
                "icons/pause.svg"
            } else {
                "icons/play.svg"
            };
            let play = div()
                .id("popout-play")
                .size(px(36.))
                .flex_none()
                .rounded_full()
                .bg(accent)
                .flex()
                .items_center()
                .justify_center()
                .cursor_pointer()
                .hover(|s| s.opacity(0.93))
                .on_click(move |_, w, cx| {
                    cx.stop_propagation();
                    on_t(Act::Toggle, w, cx)
                })
                .child(
                    svg()
                        .path(icon)
                        .size(px(24.))
                        .when(!playing && !loading, |s| s.ml(px(1.2)))
                        .text_color(c(0x0a0a0a)),
                );
            let (play_single, play_album) = if is_album { (None, Some(play)) } else { (Some(play), None) };
            let track: AnyElement = if item.kind == Kind::Video {
                seek_line(frac, accent, scrubbing, on.clone()).into_any_element()
            } else {
                wave(bars, frac, playing, accent, scrubbing, on.clone()).into_any_element()
            };
            div()
                .p(px(12.))
                .flex()
                .flex_col()
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(12.))
                        .h(px(44.))
                        .child(thumb)
                        .child(words)
                        .child(div().self_start().child(actions)),
                )
                .child(
                    div()
                        .mt(px(10.))
                        .flex()
                        .items_center()
                        .gap(px(12.))
                        .when_some(play_single, |d, play| d.child(div().w(px(44.)).flex().justify_center().child(play)))
                        .when_some(play_album, |d, play_album| {
                            let skip = |key: &'static str, icon: &'static str, act: Act| {
                                let on = on.clone();
                                div()
                                    .id(key)
                                    .size(px(30.))
                                    .flex_none()
                                    .rounded(px(8.))
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .cursor_pointer()
                                    .hover(|s| s.bg(white_a(0.07)))
                                    .on_click(move |_, w, cx| {
                                        cx.stop_propagation();
                                        on(act.clone(), w, cx)
                                    })
                                    .child(svg().path(icon).size(px(20.)).text_color(white_a(0.92)))
                            };
                            d.child(
                                div()
                                    .flex()
                                    .items_center()
                                    .gap(px(2.))
                                    .child(skip("popout-prev", "icons/skip-back.svg", Act::Prev))
                                    .child(play_album)
                                    .child(skip("popout-next", "icons/skip-forward.svg", Act::Next)),
                            )
                        })
                        .child(div().flex_1().min_w_0().child(track))
                        // Narrow, an album's times step aside for the song's controls.
                        .when(!(is_album && width < 340.), |d| {
                            d.child(
                                div()
                                    .flex_none()
                                    .text_size(px(12.))
                                    .line_height(px(24.))
                                    .text_color(white_a(0.55))
                                    .child(time),
                            )
                        }),
                )
                .children(panels)
                .into_any_element()
        };
        drop(media);

        let near = self.popout.near;
        let dragging = self.popout.dragging();
        let grips = Corner::ALL.map(|corner| {
            let on = on.clone();
            let shown = near == Some(corner);
            let mark = |d: gpui::Div| {
                let d = d.absolute().size(px(7.)).border_color(white_a(0.55));
                let d = if corner.top() { d.top(px(5.)).border_t(px(2.)) } else { d.bottom(px(5.)).border_b(px(2.)) };
                let d = if corner.left() { d.left(px(5.)).border_l(px(2.)) } else { d.right(px(5.)).border_r(px(2.)) };
                match corner {
                    Corner::Tl => d.rounded_tl(px(3.)),
                    Corner::Tr => d.rounded_tr(px(3.)),
                    Corner::Bl => d.rounded_bl(px(3.)),
                    Corner::Br => d.rounded_br(px(3.)),
                }
            };
            let g = div()
                .id(SharedString::from(format!("grip-{corner:?}")))
                .absolute()
                .size(px(18.))
                .group("popout-grip")
                .cursor(if corner.top() == corner.left() { CursorStyle::ResizeUpLeftDownRight } else { CursorStyle::ResizeUpRightDownLeft })
                .on_mouse_down(MouseButton::Left, move |e: &MouseDownEvent, w, cx| {
                    cx.stop_propagation();
                    on(Act::Press(e.position, Some(corner)), w, cx)
                })
                .child(mark(div()).opacity(if shown { 0.45 } else { 0. }).group_hover("popout-grip", |s| s.opacity(if shown { 1. } else { 0. })));
            let g = if corner.top() { g.top_0() } else { g.bottom_0() };
            if corner.left() { g.left_0() } else { g.right_0() }
        });

        let on_down = on.clone();
        let on_near = on.clone();
        let on_leave = on.clone();
        let (left, top) = (win.0 - self.popout.right - w0, win.1 - self.popout.bottom - h0);
        let tracker = {
            let on = on.clone();
            canvas(
                |_, _, _| {},
                move |_: Bounds<Pixels>, _, window, _| {
                    if !dragging {
                        return;
                    }
                    let on_move = on.clone();
                    window.on_mouse_event(move |e: &MouseMoveEvent, phase, window, cx| {
                        if phase.bubble() {
                            if e.pressed_button.is_none() {
                                on_move(Act::Release, window, cx);
                            } else {
                                on_move(Act::Drag(e.position), window, cx);
                            }
                        }
                    });
                    let on_up = on.clone();
                    window.on_mouse_event(move |_: &MouseUpEvent, phase, window, cx| {
                        if phase.bubble() {
                            on_up(Act::Release, window, cx);
                        }
                    });
                },
            )
            .absolute()
            .size_full()
        };
        let (bw, bh) = (w0, h0);
        let measured = self.popout.measured.clone();
        let measure = canvas(
            move |b: Bounds<Pixels>, window, _| {
                let h = f32::from(b.size.height);
                if measured.replace(h) != h {
                    window.request_animation_frame();
                }
            },
            |_, _, _, _| {},
        )
        .absolute()
        .size_full();
        Some(
            gpui::deferred(
                div()
                    .id("media-popout")
                    .absolute()
                    .right(px(self.popout.right))
                    .bottom(px(self.popout.bottom))
                    .w(px(bw))
                    .when(picture, |d| d.h(px(bh)))
                    .bg(c(0x161616))
                    .border_1()
                    .border_color(c(0x363636))
                    .rounded(px(14.))
                    .shadow(vec![
                        gpui::BoxShadow {
                            color: gpui::black().opacity(0.85),
                            offset: gpui::point(px(0.), px(18.)),
                            blur_radius: px(50.),
                            spread_radius: px(0.),
                            inset: false,
                        },
                        gpui::BoxShadow {
                            color: gpui::black().opacity(0.7),
                            offset: gpui::point(px(0.), px(4.)),
                            blur_radius: px(14.),
                            spread_radius: px(0.),
                            inset: false,
                        },
                        gpui::BoxShadow {
                            color: gpui::black().opacity(0.6),
                            offset: gpui::point(px(0.), px(0.)),
                            blur_radius: px(0.),
                            spread_radius: px(1.),
                            inset: false,
                        },
                    ])
                    .overflow_hidden()
                    .cursor(if dragging { CursorStyle::ClosedHand } else { CursorStyle::OpenHand })
                    .occlude()
                    .on_mouse_down(MouseButton::Left, move |e: &MouseDownEvent, w, cx| {
                        on_down(Act::Press(e.position, None), w, cx)
                    })
                    .on_mouse_move(move |e: &MouseMoveEvent, w, cx| {
                        if e.pressed_button.is_none() {
                            let (x, y) = (f32::from(e.position.x) - left, f32::from(e.position.y) - top);
                            let corner = match (y < bh - y, x < bw - x) {
                                (true, true) => Corner::Tl,
                                (true, false) => Corner::Tr,
                                (false, true) => Corner::Bl,
                                (false, false) => Corner::Br,
                            };
                            on_near(Act::Near(Some(corner)), w, cx);
                        }
                    })
                    .on_hover(move |on, w, cx| {
                        if !on {
                            on_leave(Act::Near(None), w, cx);
                        }
                    })
                    .children(glow)
                    .child(body)
                    .children(grips)
                    .child(tracker)
                    .child(measure)
                    .text_color(c(TITLE)),
            )
            .with_priority(8)
            .into_any_element(),
        )
    }
}

/// `.popout-wave`: the bars at the clock, the played part bright.
fn wave(bins: Vec<f32>, frac: f32, playing: bool, accent: gpui::Hsla, scrubbing: bool, on: On) -> impl IntoElement {
    div().h(px(26.)).w_full().child(
        canvas(
            move |bounds, window, _| window.insert_hitbox(bounds, HitboxBehavior::Normal),
            move |bounds: Bounds<Pixels>, hitbox, window, _| {
                let w = f32::from(bounds.size.width);
                let h = f32::from(bounds.size.height);
                let n = ((w / 5.).floor() as usize).max(12);
                let gap = 2.;
                let bw = (w - gap * (n - 1) as f32) / n as f32;
                for i in 0..n {
                    let v = if playing && !bins.is_empty() { bins[i * bins.len() / n] } else { 0. };
                    let val = v.max(0.15);
                    let bh = (val * h).max(3.);
                    let played = (i as f32 + 0.5) / n as f32 <= frac;
                    let alpha = (0.45 + val * 0.55) * if played { 1. } else { 0.35 };
                    let rect = Bounds::new(
                        gpui::point(bounds.origin.x + px(i as f32 * (bw + gap)), bounds.origin.y + px((h - bh) / 2.)),
                        gpui::size(px(bw), px(bh)),
                    );
                    window.paint_quad(gpui::fill(rect, accent.opacity(alpha)).corner_radii(px(1.5_f32.min(bw / 2.))));
                }
                scrub_events(bounds, &hitbox, scrubbing, on.clone(), window);
            },
        )
        .size_full(),
    )
}

/// `.popout-track`: a sound-only video's place in it.
fn seek_line(frac: f32, accent: gpui::Hsla, scrubbing: bool, on: On) -> impl IntoElement {
    div().h(px(18.)).w_full().child(
        canvas(
            move |bounds, window, _| window.insert_hitbox(bounds, HitboxBehavior::Normal),
            move |bounds: Bounds<Pixels>, hitbox, window, _| {
                let mid = bounds.center().y;
                let line = Bounds::new(gpui::point(bounds.origin.x, mid - px(2.)), gpui::size(bounds.size.width, px(4.)));
                window.paint_quad(gpui::fill(line, c(0x363636)));
                let fill = Bounds::new(line.origin, gpui::size(bounds.size.width * frac, px(4.)));
                window.paint_quad(gpui::fill(fill, accent).corner_radii(px(2.)));
                scrub_events(bounds, &hitbox, scrubbing, on.clone(), window);
            },
        )
        .size_full(),
    )
}

fn scrub_events(bounds: Bounds<Pixels>, hitbox: &gpui::Hitbox, scrubbing: bool, on: On, window: &mut Window) {
    window.set_cursor_style(CursorStyle::PointingHand, hitbox);
    let at = move |x: Pixels| (f32::from(x - bounds.origin.x) / f32::from(bounds.size.width).max(1.)).clamp(0., 1.);
    {
        let (on, hitbox) = (on.clone(), hitbox.clone());
        window.on_mouse_event(move |e: &MouseDownEvent, phase, window, cx| {
            if phase.bubble() && e.button == MouseButton::Left && hitbox.is_hovered(window) {
                cx.stop_propagation();
                on(Act::Seek(at(e.position.x), false), window, cx);
            }
        });
    }
    if !scrubbing {
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
}
