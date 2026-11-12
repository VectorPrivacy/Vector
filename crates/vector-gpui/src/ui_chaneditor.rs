//! Channel Settings and channel deletion, on `VectorApp`.

use std::time::{Duration, Instant};

use gpui::prelude::*;
use gpui::{Context, Focusable, FontWeight, Window, px};

use super::{PopupAct, VectorApp};
use crate::backend;
use crate::chaneditor::{Act, ChannelEditor, Perms, Saved};
use crate::composer::{CancelInput, InputChanged, SendText, TextInput};
use crate::kit;
use crate::theme::*;

impl VectorApp {
    pub(super) fn open_channel_editor(
        &mut self,
        community: String,
        channel: String,
        is_category: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let (name, topic, view, private, primary) = {
            let store = self.store.borrow();
            let Some(com) = store.community(&community) else { return };
            if is_category {
                let Some(cat) = com.categories.iter().find(|k| k.category_id == channel) else { return };
                (cat.name.clone(), String::new(), "chat".to_string(), false, false)
            } else {
                let Some(ch) = com.channel(&channel) else { return };
                let view = if ch.view.is_empty() { "chat" } else { ch.view.as_str() };
                (
                    ch.name.clone(),
                    ch.topic.clone().unwrap_or_default(),
                    view.to_string(),
                    ch.private,
                    com.primary_channel.as_deref() == Some(channel.as_str()),
                )
            }
        };
        let look = |size: f32, line: f32, weight: FontWeight| crate::composer::Look {
            size: px(size),
            line_height: px(line),
            weight,
            ..kit::field_look()
        };
        let search = cx.new(|cx| {
            TextInput::new("Search", cx).with_look(crate::composer::Look {
                placeholder: c(0xb2b2b2),
                ..look(14., 17., FontWeight::NORMAL)
            })
        });
        let name_in = cx.new(|cx| {
            let mut t = TextInput::new("", cx).with_look(look(15., 18., FontWeight::MEDIUM)).keep_on_submit();
            t.set_text(&name, cx);
            t
        });
        let topic_in = cx.new(|cx| {
            let mut t = TextInput::new("Shown at the top of the channel", cx)
                .with_look(crate::composer::Look {
                    placeholder: c(0x5c5c5c),
                    ..look(15., 22.5, FontWeight::NORMAL)
                })
                .multiline(px(92.));
            t.set_text(&topic, cx);
            t
        });
        let add_query = cx.new(|cx| {
            TextInput::new("Find a role or member", cx)
                .with_look(look(14., 17., FontWeight::NORMAL))
                .keep_on_submit()
        });
        for input in [&search, &name_in, &topic_in, &add_query] {
            cx.subscribe(input, |_, _, _: &InputChanged, cx| cx.notify()).detach();
        }
        for input in [&search, &name_in, &topic_in] {
            cx.subscribe_in(input, window, |app, _, _: &CancelInput, window, cx| {
                app.on_channel_editor(Act::Close, window, cx)
            })
            .detach();
        }
        cx.subscribe_in(&add_query, window, |app, _, _: &CancelInput, window, cx| {
            app.on_channel_editor(Act::CloseAdd, window, cx)
        })
        .detach();
        cx.subscribe_in(&name_in, window, |app, _, _: &SendText, window, cx| {
            if app.channel_editor.as_ref().is_some_and(|m| m.valid(cx)) {
                app.on_channel_editor(Act::Save, window, cx)
            }
        })
        .detach();
        cx.subscribe_in(&add_query, window, |app, _, _: &SendText, window, cx| {
            let first = app.channel_editor.as_ref().and_then(|m| {
                let store = app.store.borrow();
                let (roles, members) = m.addable(&store, cx);
                roles
                    .first()
                    .map(|r| format!("role:{}", r.id))
                    .or_else(|| members.first().map(|m| format!("member:{}", m.0)))
            });
            if let Some(key) = first {
                app.on_channel_editor(Act::Pick(key), window, cx)
            }
        })
        .detach();
        self.channel_editor = Some(ChannelEditor {
            community,
            channel,
            is_category,
            primary,
            section: if is_category { "permissions" } else { "overview" },
            anchor: None,
            search,
            scroll: gpui::ScrollHandle::new(),
            name: name_in,
            topic: topic_in,
            view: view.clone(),
            private,
            saved: Saved { name, topic, view },
            perms: None,
            target: "everyone".into(),
            draft: (0, 0),
            busy: false,
            error: None,
            warn_until: None,
            warn_switch: false,
            adding: false,
            add_closed: None,
            add_query,
        });
        self.load_channel_perms(cx);
        cx.notify();
    }

    fn load_channel_perms(&mut self, cx: &mut Context<Self>) {
        let Some(m) = self.channel_editor.as_mut() else { return };
        let v = if self.live {
            backend::community::channel_permissions(&m.community, &m.channel).ok()
        } else {
            match m.perms.take() {
                Some(p) => {
                    m.perms = Some(p);
                    None
                }
                None => self.store.borrow().fixture_channel_perms.clone(),
            }
        };
        if let Some(v) = v {
            let perms = Perms::parse(&v);
            m.draft = perms.stored(&m.target);
            m.perms = Some(perms);
        } else if let Some(p) = m.perms.as_ref() {
            m.draft = p.stored(&m.target);
        }
        cx.notify();
    }

    fn warn_channel_editor(&mut self, switch: bool, cx: &mut Context<Self>) {
        let Some(m) = self.channel_editor.as_mut() else { return };
        m.warn_switch = switch;
        m.warn_until = Some(Instant::now() + Duration::from_millis(1600));
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(Duration::from_millis(1700)).await;
            this.update(cx, |_, cx| cx.notify()).ok();
        })
        .detach();
    }

    /// Runs `rx`, then reloads the permissions; a failure stays on the save bar.
    fn channel_editor_after(
        &mut self,
        rx: futures::channel::oneshot::Receiver<Result<(), String>>,
        then: impl FnOnce(&mut ChannelEditor) + 'static,
        cx: &mut Context<Self>,
    ) {
        let key = self.channel_editor.as_ref().map(|m| (m.community.clone(), m.channel.clone()));
        cx.spawn(async move |this, cx| {
            let res = rx.await;
            this.update(cx, |this, cx| {
                let Some(m) = this.channel_editor.as_mut().filter(|m| Some((m.community.clone(), m.channel.clone())) == key) else {
                    return;
                };
                m.busy = false;
                match res {
                    Ok(Ok(())) => {
                        m.error = None;
                        then(m);
                    }
                    Ok(Err(e)) => m.error = Some(e),
                    Err(_) => m.error = Some("Cancelled".into()),
                }
                this.load_channel_perms(cx);
            })
            .ok();
        })
        .detach();
    }

    pub(super) fn on_channel_editor(&mut self, act: Act, window: &mut Window, cx: &mut Context<Self>) {
        let live = self.live;
        let Some(m) = self.channel_editor.as_mut() else { return };
        match act {
            Act::Close => {
                if m.adding {
                    m.adding = false;
                    m.add_closed = Some(Instant::now());
                } else if m.busy || m.dirty(cx) {
                    self.warn_channel_editor(false, cx);
                } else {
                    self.channel_editor = None;
                }
            }
            Act::Section(id) => {
                if m.section != id {
                    m.section = id;
                    m.anchor = None;
                    m.adding = false;
                    m.scroll.set_offset(gpui::point(px(0.), px(0.)));
                }
            }
            Act::Anchor(section, anchor) => {
                if m.section != section {
                    m.section = section;
                    m.adding = false;
                }
                m.anchor = Some(anchor);
                let ix = m
                    .sections()
                    .iter()
                    .find(|s| s.id == section)
                    .and_then(|s| s.anchors.iter().position(|a| a.id == anchor))
                    .unwrap_or(0);
                if ix == 0 {
                    m.scroll.set_offset(gpui::point(px(0.), px(0.)));
                } else {
                    m.scroll.scroll_to_item(ix);
                }
            }
            Act::ClearSearch => m.search.update(cx, |t, cx| t.set_text("", cx)),
            Act::View(v) => m.view = v.into(),
            Act::Private(want) => {
                if m.busy {
                    return;
                }
                if !live {
                    m.private = want;
                    let (com, ch) = (m.community.clone(), m.channel.clone());
                    if let Some(c) = self.store.borrow_mut().communities.iter_mut().find(|c| c.community_id == com)
                        && let Some(ch) = c.channels.iter_mut().find(|x| x.channel_id == ch)
                    {
                        ch.private = want;
                    }
                } else {
                    m.busy = true;
                    let rx = backend::run(backend::community::set_channel_private(m.community.clone(), m.channel.clone(), want));
                    self.channel_editor_after(rx, move |m| m.private = want, cx);
                }
            }
            Act::Sync(on) => {
                if m.busy {
                    return;
                }
                let category = on.then(|| m.perms.as_ref().and_then(|p| p.category.clone())).flatten();
                if !live {
                    if let Some(p) = m.perms.as_mut() {
                        p.synced = on;
                    }
                } else {
                    m.busy = true;
                    let rx = backend::run(backend::community::set_channel_sync(m.community.clone(), m.channel.clone(), category));
                    self.channel_editor_after(rx, |_| {}, cx);
                }
            }
            Act::Target(key) | Act::Pick(key) => {
                m.adding = false;
                m.add_query.update(cx, |t, cx| t.set_text("", cx));
                if key != m.target && m.overwrite_dirty() {
                    self.warn_channel_editor(true, cx);
                } else if let Some(p) = m.perms.as_ref() {
                    m.draft = p.stored(&key);
                    m.target = key;
                }
            }
            Act::Bit(bit, to) => {
                if m.can_touch(bit) && !m.busy {
                    m.set_bit(bit, to);
                }
            }
            Act::ToggleAdd => {
                if m.add_closed.is_some_and(|t| t.elapsed() < Duration::from_millis(250)) {
                    return;
                }
                m.adding = !m.adding;
                if m.adding {
                    m.add_query.update(cx, |t, cx| t.set_text("", cx));
                    window.focus(&m.add_query.focus_handle(cx), cx);
                }
            }
            Act::CloseAdd => {
                if m.adding {
                    m.adding = false;
                    m.add_closed = Some(Instant::now());
                    m.add_query.update(cx, |t, cx| t.set_text("", cx));
                }
            }
            Act::Reset => {
                let (name, topic) = (m.saved.name.clone(), m.saved.topic.clone());
                m.name.update(cx, |t, cx| t.set_text(&name, cx));
                m.topic.update(cx, |t, cx| t.set_text(&topic, cx));
                m.view = m.saved.view.clone();
                m.error = None;
                if let Some(p) = m.perms.as_ref() {
                    m.draft = p.stored(&m.target);
                }
            }
            Act::Save => {
                if m.busy || !m.dirty(cx) || !m.valid(cx) {
                    return;
                }
                let details = m.overview_dirty(cx).then(|| backend::community::ChannelDetails {
                    name: m.name.read(cx).text().trim().to_string(),
                    topic: Some(m.topic.read(cx).text().trim().to_string()).filter(|t| !t.is_empty() && !m.is_category),
                    view: m.view.clone(),
                    category: m.is_category,
                });
                let overwrite = m.overwrite_dirty().then(|| {
                    let (kind, id) = match m.target.split_once(':') {
                        Some((k, id)) => (k.to_string(), Some(id.to_string())),
                        None => ("everyone".to_string(), None),
                    };
                    (kind, id, m.draft.0.to_string(), m.draft.1.to_string())
                });
                let saved = details.as_ref().map(|d| Saved {
                    name: d.name.clone(),
                    topic: d.topic.clone().unwrap_or_default(),
                    view: d.view.clone(),
                });
                let apply = move |m: &mut ChannelEditor| {
                    if let Some(s) = saved {
                        m.saved = s;
                    }
                };
                if !live {
                    let (com, ch, target, draft) = (m.community.clone(), m.channel.clone(), m.target.clone(), m.draft);
                    if let Some(d) = details.as_ref() {
                        let mut store = self.store.borrow_mut();
                        if let Some(c) = store.communities.iter_mut().find(|c| c.community_id == com) {
                            if d.category {
                                if let Some(k) = c.categories.iter_mut().find(|k| k.category_id == ch) {
                                    k.name = d.name.clone();
                                }
                            } else if let Some(x) = c.channels.iter_mut().find(|x| x.channel_id == ch) {
                                x.name = d.name.clone();
                                x.topic = d.topic.clone();
                                x.view = d.view.clone();
                            }
                        }
                    }
                    if overwrite.is_some()
                        && let Some(p) = m.perms.as_mut()
                    {
                        p.overwrites.retain(|o| o.key != target);
                        if draft != (0, 0) {
                            p.overwrites.push(crate::chaneditor::Overwrite { key: target, npub: None, allow: draft.0, deny: draft.1 });
                        }
                    }
                    let (name, topic) = (m.name.read(cx).text().trim().to_string(), m.topic.read(cx).text().trim().to_string());
                    m.name.update(cx, |t, cx| t.set_text(&name, cx));
                    m.topic.update(cx, |t, cx| t.set_text(&topic, cx));
                    apply(m);
                } else {
                    m.busy = true;
                    m.error = None;
                    let rx = backend::run(backend::community::save_channel(
                        m.community.clone(),
                        m.channel.clone(),
                        backend::community::ChannelSave { details, overwrite },
                    ));
                    self.channel_editor_after(rx, apply, cx);
                }
            }
            Act::Delete => {
                if m.busy {
                    return;
                }
                let (community, channel, category) = (m.community.clone(), m.channel.clone(), m.is_category);
                self.ask_delete_channel(community, channel, category, cx);
            }
        }
        cx.notify();
    }

    pub(super) fn ask_delete_channel(&mut self, community: String, channel: String, category: bool, cx: &mut Context<Self>) {
        let text = {
            let store = self.store.borrow();
            let Some(com) = store.community(&community) else { return };
            if category {
                let Some(k) = com.categories.iter().find(|k| k.category_id == channel) else { return };
                format!("Delete <b>{}</b>? Its channels stay, uncategorized.", k.name)
            } else {
                let Some(ch) = com.channel(&channel) else { return };
                format!(
                    "Delete <b>{}{}</b>? It's deleted for everyone, with all its messages and files.",
                    if ch.is_voice() { "" } else { "#" },
                    ch.name
                )
            }
        };
        let mut popup = kit::Popup::notice(if category { "Delete category" } else { "Delete channel" });
        popup.text = Some(text.into());
        popup.confirm = "Confirm".into();
        popup.cancel = Some("Cancel".into());
        self.popup = Some((popup, PopupAct::DeleteChannel { community, channel, category }));
        cx.notify();
    }

    pub(super) fn delete_channel(&mut self, community: String, channel: String, category: bool, cx: &mut Context<Self>) {
        let id = channel.clone();
        let gone = move |this: &mut Self| {
            if this.channel_editor.as_ref().is_some_and(|m| m.channel == id) {
                this.channel_editor = None;
            }
            if !category && this.open.as_deref() == Some(id.as_str()) {
                this.open = None;
            }
        };
        if !self.live {
            let mut store = self.store.borrow_mut();
            if let Some(c) = store.communities.iter_mut().find(|c| c.community_id == community) {
                if category {
                    c.categories.retain(|k| k.category_id != channel);
                    for ch in c.channels.iter_mut().filter(|ch| ch.category_id.as_deref() == Some(channel.as_str())) {
                        ch.category_id = None;
                    }
                } else {
                    c.channels.retain(|ch| ch.channel_id != channel);
                }
            }
            drop(store);
            gone(self);
            cx.notify();
            return;
        }
        let rx = if category {
            backend::run(backend::community::delete_category(community, channel.clone()))
        } else {
            backend::run(backend::community::delete_channel(community, channel.clone()))
        };
        cx.spawn(async move |this, cx| {
            let res = rx.await;
            this.update(cx, |this, cx| {
                match res {
                    Ok(Ok(())) => gone(this),
                    Ok(Err(e)) => this.notice(
                        if category { "Couldn't delete the category" } else { "Couldn't delete the channel" },
                        e,
                        cx,
                    ),
                    Err(_) => {}
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }
}
