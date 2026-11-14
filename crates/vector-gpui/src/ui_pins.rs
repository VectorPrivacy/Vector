//! `pins.js`: a channel's pins follow the open chat, refresh on `community_pins_updated`, and
//! are made and taken down from the message menu or the drawer's ✕.

use std::rc::Rc;

use gpui::{Context, Window};

use super::VectorApp;
use crate::backend;
use crate::pins::{Act, Pin};

impl VectorApp {
    /// `pinsOnChatOpened`: pins exist only on Concord v2 channels (64-hex community id).
    pub(super) fn pins_opened(&mut self, cx: &mut Context<Self>) {
        self.pins.open = false;
        self.pins.expanded.clear();
        let Some(chat) = self.open.clone() else { return };
        let community = self.store.borrow().community_of(&chat).map(|c| c.community_id.clone()).filter(|c| c.len() == 64);
        let Some(community) = community else {
            self.pins.channel.clear();
            return;
        };
        if self.pins.channel != chat.as_ref() {
            self.pins.list.clear();
            self.pins.sealed = false;
            self.pins.previews.clear();
            self.pins.clips.borrow_mut().clear();
        }
        self.pins.can_pin = self.community_caps(&community).contains("pin_messages");
        self.pins.community = community;
        self.pins.channel = chat.to_string();
        self.pins_refresh(cx);
    }

    pub(super) fn pins_on(&self, cx: &mut Context<Self>) -> crate::pins::OnAct {
        let app = cx.entity().downgrade();
        Rc::new(move |act, window, cx| {
            app.update(cx, |app, cx| app.on_pins(act, window, cx)).ok();
        })
    }

    /// `pinsRefresh`: the locally folded head, each pin's jumpability resolved before it shows.
    pub(super) fn pins_refresh(&mut self, cx: &mut Context<Self>) {
        let (community, channel) = (self.pins.community.clone(), self.pins.channel.clone());
        if channel.is_empty() {
            return;
        }
        let fetched = if self.live {
            backend::community::channel_pins(&community, &channel).unwrap_or_default()
        } else {
            self.store.borrow().fixture_pins.get(&channel).cloned().map(|p| serde_json::json!({ "pins": p })).unwrap_or_default()
        };
        let mut list: Vec<Pin> = fetched["pins"].as_array().into_iter().flatten().filter_map(Pin::from_json).collect();
        {
            let store = self.store.borrow();
            let held = store.chat(&channel).map(|c| &c.messages);
            for p in &mut list {
                p.jumpable = held.is_some_and(|m| m.iter().any(|m| m.id == p.id));
            }
        }
        let probe: Vec<String> = list.iter().filter(|p| !p.jumpable).map(|p| p.id.clone()).collect();
        self.pins.sealed = fetched["sealed"].as_bool().unwrap_or(false);
        self.pins.list = list;
        let show = !self.pins.list.is_empty() || self.pins.sealed;
        self.pins.shown.insert(channel.clone(), show);
        cx.notify();
        if !self.live || probe.is_empty() {
            return;
        }
        let chat = channel.clone();
        let rx = backend::run(async move {
            let mut found = Vec::new();
            for id in probe {
                // A zero-width window returns nothing, anchor included.
                let slice = backend::messaging::messages_around(chat.clone(), id.clone(), 1, 1).await.unwrap_or_default();
                if slice.iter().any(|m| m.id == id) {
                    found.push(id);
                }
            }
            found
        });
        cx.spawn(async move |this, cx| {
            let Ok(found) = rx.await else { return };
            this.update(cx, |this, cx| {
                if this.pins.channel != channel {
                    return;
                }
                for p in this.pins.list.iter_mut().filter(|p| found.contains(&p.id)) {
                    p.jumpable = true;
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn on_pins(&mut self, act: Act, window: &mut Window, cx: &mut Context<Self>) {
        match act {
            Act::Toggle => self.pins.open = !self.pins.open,
            Act::Close => self.pins.open = false,
            Act::Row(id) => {
                let Some(pin) = self.pins.list.iter().find(|p| p.id == id) else { return };
                if pin.jumpable {
                    self.pins.open = false;
                    self.jump_or_say(id, "That message isn't in your local history", cx);
                } else if self.pins.clips.borrow().get(&id).is_some_and(|c| c.get()) || pin.media.is_some() || pin.content.lines().filter(|l| !l.trim().is_empty()).count() > 1 {
                    self.pins_expand(id, cx);
                }
            }
            Act::Expand(id) => self.pins_expand(id, cx),
            Act::Open(id) => self.pins_open_file(id, cx),
            Act::Unpin(id) => self.set_pinned(id, false, cx),
            Act::Text(id, t) => self.on_text(id, t, window, cx),
        }
        cx.notify();
    }

    fn pins_expand(&mut self, id: String, cx: &mut Context<Self>) {
        if !self.pins.expanded.remove(&id) {
            self.pins.expanded.insert(id.clone());
            let image = self.pins.list.iter().any(|p| p.id == id && p.media == Some("image"));
            if image && self.live && !self.pins.previews.contains_key(&id) {
                let (community, channel) = (self.pins.community.clone(), self.pins.channel.clone());
                let rx = backend::run(backend::community::pinned_attachment(community, channel, id.clone()));
                cx.spawn(async move |this, cx| {
                    let Ok(Ok(att)) = rx.await else { return };
                    let Some(path) = att["path"].as_str().filter(|p| !p.starts_with("vmedia:")).map(std::path::PathBuf::from) else {
                        return;
                    };
                    this.update(cx, |this, cx| {
                        this.pins.previews.insert(id, path);
                        cx.notify();
                    })
                    .ok();
                })
                .detach();
            }
        }
    }

    /// The pin's file, fetched from the proof alone, then revealed or saved like the chat's own.
    fn pins_open_file(&mut self, id: String, cx: &mut Context<Self>) {
        if !self.live {
            return;
        }
        let (community, channel) = (self.pins.community.clone(), self.pins.channel.clone());
        let rx = backend::run(backend::community::pinned_attachment(community, channel, id));
        cx.spawn(async move |this, cx| {
            let res = rx.await;
            this.update(cx, |this, cx| match res {
                Ok(Ok(att)) => {
                    let Some(path) = att["path"].as_str() else { return };
                    let name = att["name"].as_str().unwrap_or_default().to_string();
                    match path.strip_prefix("vmedia:") {
                        Some(hash) => this.save_resident(hash.to_string(), name, cx),
                        None => cx.reveal_path(std::path::Path::new(path)),
                    }
                }
                Ok(Err(e)) => this.show_toast(&e, cx),
                Err(_) => {}
            })
            .ok();
        })
        .detach();
    }

    /// `pinsPin` / `pinsUnpin`.
    pub(super) fn set_pinned(&mut self, id: String, on: bool, cx: &mut Context<Self>) {
        if !self.live {
            return;
        }
        let (community, channel) = (self.pins.community.clone(), self.pins.channel.clone());
        let rx = backend::run(backend::community::set_pinned(community, channel, id, on));
        cx.spawn(async move |this, cx| {
            let res = rx.await;
            this.update(cx, |this, cx| {
                match res {
                    Ok(Ok(())) if on => this.show_toast("Message pinned", cx),
                    Ok(Err(e)) => this.show_toast(&e, cx),
                    _ => {}
                }
                this.pins_refresh(cx);
            })
            .ok();
        })
        .detach();
    }
}
