//! Voice Channels: joining and leaving, mute and deafen from anywhere, and a member's menu.

use gpui::{Context, Pixels, Point, Window};
use serde_json::Value;

use super::{CHROME_H, MenuAct, PopupAct, VectorApp};
use crate::voice::{Act, Member, Session};
use crate::{backend, kit};

impl VectorApp {
    /// What a session holds now: ours, live; fixtures under VECTOR_VOICE.
    pub(super) fn load_voice_calls(&mut self, cx: &mut Context<Self>) {
        if self.live {
            self.voice_state(backend::voice::state(), cx);
            return;
        }
        let Some(fix) = self.store.borrow().fixture_voice.clone() else { return };
        if let Some(rooms) = fix.get("present").and_then(Value::as_object) {
            for (ch, list) in rooms {
                let list: Vec<Member> = serde_json::from_value(list.clone()).unwrap_or_default();
                self.voice.present.insert(ch.clone(), list);
            }
        }
        let session = fix.get("session").cloned().and_then(|s| serde_json::from_value::<Session>(s).ok());
        self.voice.set_session(session);
        self.voice_tick(cx);
    }

    fn voice_state(&mut self, s: Option<Value>, cx: &mut Context<Self>) {
        let before = self.voice.session.as_ref().map(|s| s.channel_id.clone());
        let session = s.and_then(|s| serde_json::from_value::<Session>(s).ok());
        let now = session.as_ref().map(|s| s.channel_id.clone());
        self.voice.set_session(session);
        if before != now
            && let Some(ch) = before
        {
            self.voice_present(&ch);
        }
        if let Some(ch) = now {
            self.voice_present(&ch);
        }
        self.voice_tick(cx);
    }

    pub(super) fn voice_present(&mut self, channel: &str) {
        if !self.live {
            return;
        }
        let list: Vec<Member> = serde_json::from_value(backend::voice::present(channel)).unwrap_or_default();
        self.voice.present.insert(channel.to_string(), list);
    }

    pub(super) fn voice_event(&mut self, name: &str, payload: &Value, cx: &mut Context<Self>) {
        match name {
            "voice_state" => self.voice_state((!payload.is_null()).then(|| payload.clone()), cx),
            "voice_levels" => {
                if let Ok(s) = serde_json::from_value::<Session>(payload.clone()) {
                    self.voice.set_session(Some(s));
                }
            }
            "voice_presence" => {
                if let Some(ch) = payload.get("channel_id").and_then(Value::as_str) {
                    self.voice_present(ch);
                }
            }
            _ => return,
        }
        cx.notify();
    }

    /// The bar's clock ticks while we are in a call.
    fn voice_tick(&mut self, cx: &mut Context<Self>) {
        if self.voice_ticking || self.voice.session.is_none() {
            return;
        }
        self.voice_ticking = true;
        cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(std::time::Duration::from_secs(1)).await;
                let going = this
                    .update(cx, |this, cx| {
                        if this.voice.session.is_none() {
                            this.voice_ticking = false;
                        } else {
                            cx.notify();
                        }
                        this.voice_ticking
                    })
                    .unwrap_or(false);
                if !going {
                    break;
                }
            }
        })
        .detach();
    }

    pub(super) fn on_voice(&mut self, act: Act, window: &mut Window, cx: &mut Context<Self>) {
        match act {
            Act::Join(community, channel) => self.voice_join(community, channel, cx),
            Act::Leave => {
                if self.live {
                    let _ = backend::run(backend::voice::leave());
                } else {
                    self.voice.set_session(None);
                }
            }
            Act::Mic => self.voice_mic(cx),
            Act::Deafen => {
                let Some(s) = self.voice.session.as_ref() else { return };
                let on = !s.deafened;
                self.voice_set(None, Some(on), cx);
            }
            Act::Menu(community, channel, npub, at) => self.voice_menu(community, channel, npub, at, cx),
            Act::Profile(npub, community, key) => {
                let at = self.anchors.borrow().get(&key).copied();
                self.show_mini(npub, Some(community), at, cx);
            }
            Act::Open(community, channel) => {
                if self.community.as_ref().map(|c| c.id.as_str()) != Some(community.as_str()) {
                    self.open_community(community, window, cx);
                }
                self.open_channel(channel, window, cx);
            }
            Act::Move(p) => {
                self.call.pos = Some(crate::call::clamp(p, self.voice.bar_size(), window.viewport_size(), CHROME_H));
            }
            Act::Moved => {
                if let Some(p) = self.call.pos {
                    backend::device::set(
                        "call_pill_pos",
                        serde_json::json!({ "x": f32::from(p.x), "y": f32::from(p.y) }).to_string(),
                    );
                }
            }
            Act::Camera(on) => self.set_video(0, on, cx),
            Act::Share(on) => self.set_video(1, on, cx),
            Act::Pick(npub) => {
                let me = self.store.borrow().my_npub.clone();
                let ch = self
                    .voice
                    .session
                    .as_ref()
                    .map(|s| s.channel_id.clone())
                    .unwrap_or_default();
                let viewing = self.voice.viewing(&ch, &me);
                self.voice.focus = Some(if viewing.as_deref() == Some(npub.as_str()) {
                    String::new()
                } else {
                    npub
                });
            }
            Act::HideScreen => self.voice.focus = Some(String::new()),
            Act::Expand => self.voice.expanded = !self.voice.expanded,
            Act::MicAccess => {
                let where_ = if cfg!(target_os = "macos") {
                    "System Settings → Privacy & Security → Microphone"
                } else if cfg!(windows) {
                    "Settings → Privacy → Microphone"
                } else {
                    "your device's app settings"
                };
                self.notice(
                    "Microphone Access",
                    format!("Vector doesn't have access to your microphone. Allow it in {where_}, then try again."),
                    cx,
                );
            }
        }
        cx.notify();
    }

    /// Deafened, the mic undeafens and unmutes; under a moderator, it asks and is told why not.
    pub(super) fn voice_mic(&mut self, cx: &mut Context<Self>) {
        let Some(s) = self.voice.session.as_ref() else { return };
        if s.deafened {
            let unmute = s.muted.then_some(false);
            self.voice_set(unmute, Some(false), cx);
        } else if s.server_muted || s.server_deafened {
            self.voice_set(Some(false), None, cx);
        } else {
            let on = !s.muted;
            self.voice_set(Some(on), None, cx);
        }
    }

    fn voice_set(&mut self, muted: Option<bool>, deafened: Option<bool>, cx: &mut Context<Self>) {
        if !self.live {
            if let Some(s) = self.voice.session.as_mut() {
                if let Some(m) = muted {
                    s.muted = m;
                }
                if let Some(d) = deafened {
                    s.deafened = d;
                }
            }
            cx.notify();
            return;
        }
        let rx = backend::run(async move {
            if let Some(d) = deafened {
                backend::voice::set_deafened(d)?;
            }
            if let Some(m) = muted {
                backend::voice::set_muted(m)?;
            }
            Ok(())
        });
        self.toast_err(rx, cx);
    }

    fn voice_join(&mut self, community: String, channel: String, cx: &mut Context<Self>) {
        if !self.live || self.voice.joining.is_some() {
            return;
        }
        if vector_core::transport::realtime::check()
            .is_err_and(|e| e == vector_core::transport::realtime::CONSENT_REQUIRED)
        {
            let label = backend::calls::off_clearnet_label().unwrap_or_else(|| "the network in use".into());
            let mut p = kit::Popup::notice(format!("Connect outside {label}?"));
            p.icon = None;
            p.text = Some("Calls and multiplayer Mini Apps connect directly.<br>The people you connect with and the iroh relay can see your IP address.<br>Allowed until you close Vector, switch accounts or change networks.".into());
            p.confirm = "Connect".into();
            p.cancel = Some("Cancel".into());
            self.popup = Some((
                p,
                PopupAct::RealtimeThen(Box::new(move |app, cx| app.voice_join(community, channel, cx))),
            ));
            cx.notify();
            return;
        }
        self.voice.joining = Some(channel.clone());
        let rx = backend::run(backend::voice::join(community, channel));
        cx.spawn(async move |this, cx| {
            let out = rx.await;
            this.update(cx, |this, cx| {
                this.voice.joining = None;
                match out {
                    Ok(Ok(state)) => this.voice_state(Some(state), cx),
                    Ok(Err(e)) => this.show_toast(&e, cx),
                    Err(_) => {}
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
        cx.notify();
    }

    fn voice_menu(&mut self, community: String, channel: String, npub: String, at: Point<Pixels>, cx: &mut Context<Self>) {
        let me = self.store.borrow().my_npub.clone();
        if npub == me {
            return;
        }
        let volume = self
            .voice
            .session
            .as_ref()
            .filter(|s| s.channel_id == channel)
            .and_then(|s| s.peers.iter().find(|p| p.npub == npub))
            .map_or(1., |p| p.volume);
        self.voice_present(&channel);
        let them = self.voice.present(&channel).iter().find(|p| p.npub == npub).cloned();
        let (mute, deafen, move_) = if self.live { backend::voice::powers(&community, &npub) } else { (true, true, true) };
        let mut rows: Vec<(kit::MenuItem, MenuAct)> = vec![
            if volume == 0. {
                (kit::MenuItem::new("Unmute for Me", "volume-max"), MenuAct::VoiceVolume(npub.clone(), 1.))
            } else {
                (kit::MenuItem::new("Mute for Me", "volume-mute"), MenuAct::VoiceVolume(npub.clone(), 0.))
            },
            (
                kit::MenuItem::slider("Volume", volume / 2., format!("{}%", (volume * 100.).round())),
                MenuAct::VoiceSlide(npub.clone()),
            ),
        ];
        let mut mods: Vec<(kit::MenuItem, MenuAct)> = Vec::new();
        let act = |action: &'static str, to: Option<String>| MenuAct::VoiceModerate {
            community: community.clone(),
            channel: channel.clone(),
            npub: npub.clone(),
            action,
            to,
        };
        if let Some(them) = &them {
            if mute {
                mods.push(if them.server_muted {
                    (kit::MenuItem::new("Server Unmute", "mic-on"), act("unmute", None))
                } else {
                    (kit::MenuItem::new("Server Mute", "mic-off"), act("mute", None))
                });
            }
            if deafen {
                mods.push(if them.server_deafened {
                    (kit::MenuItem::new("Server Undeafen", "volume-max"), act("undeafen", None))
                } else {
                    (kit::MenuItem::new("Server Deafen", "volume-mute"), act("deafen", None))
                });
            }
            if move_ {
                let others: Vec<(String, String)> = self
                    .store
                    .borrow()
                    .community(&community)
                    .map(|c| {
                        c.channels
                            .iter()
                            .filter(|ch| ch.is_voice() && ch.channel_id != channel)
                            .map(|ch| (ch.channel_id.clone(), ch.name.clone()))
                            .collect()
                    })
                    .unwrap_or_default();
                for (id, name) in others {
                    mods.push((kit::MenuItem::new(format!("Move to {name}"), "volume-max"), act("move", Some(id))));
                }
                mods.push((kit::MenuItem::new("Disconnect", "x").danger(), act("disconnect", None)));
            }
        }
        if !mods.is_empty() {
            rows.push((kit::MenuItem::divider(), MenuAct::Nothing));
            rows.extend(mods);
        }
        rows.push((kit::MenuItem::divider(), MenuAct::Nothing));
        rows.push((kit::MenuItem::new("View Profile", "user-circle"), MenuAct::ViewProfile(npub)));
        self.show_menu(at, rows, cx);
    }

    pub(super) fn voice_menu_act(&mut self, act: MenuAct, cx: &mut Context<Self>) {
        match act {
            MenuAct::VoiceVolume(npub, v) => self.voice_volume(&npub, v, cx),
            MenuAct::VoiceModerate { community, channel, npub, action, to } => {
                if self.live {
                    let rx = backend::run(backend::voice::moderate(community, channel, npub, action, to));
                    self.toast_err(rx, cx);
                }
            }
            MenuAct::ViewProfile(npub) => self.open_profile(npub, cx),
            _ => {}
        }
    }

    fn voice_volume(&mut self, npub: &str, v: f32, cx: &mut Context<Self>) {
        if let Some(p) = self.voice.session.as_mut().and_then(|s| s.peers.iter_mut().find(|p| p.npub == npub)) {
            p.volume = v;
        }
        if self.live
            && let Err(e) = backend::voice::set_volume(npub, v)
        {
            self.show_toast(&e, cx);
        }
    }

    /// A menu slider moved: the row's value follows, and what it drives applies at once.
    pub(super) fn on_menu_slide(&mut self, ix: usize, frac: f32, cx: &mut Context<Self>) {
        let Some((_, items, acts)) = self.menu.as_mut() else { return };
        let Some(MenuAct::VoiceSlide(npub)) = acts.get(ix) else { return };
        let npub = npub.clone();
        let pct = (frac * 200. / 5.).round() * 5.;
        if let Some(item) = items.get_mut(ix) {
            item.slider = Some((pct / 200., format!("{pct}%").into()));
        }
        self.voice_volume(&npub, pct / 100., cx);
        cx.notify();
    }

    /// Cmd/Ctrl+Shift+M and +D, from anywhere in the app while in a voice call.
    pub(super) fn voice_key(&mut self, e: &gpui::KeyDownEvent, cx: &mut Context<Self>) -> bool {
        let m = &e.keystroke.modifiers;
        if !(m.control || m.platform) || !m.shift || m.alt || self.voice.session.is_none() {
            return false;
        }
        match e.keystroke.key.as_str() {
            "m" => self.voice_mic(cx),
            "d" => {
                let on = !self.voice.session.as_ref().is_some_and(|s| s.deafened);
                self.voice_set(None, Some(on), cx);
            }
            _ => return false,
        }
        cx.notify();
        true
    }
}
