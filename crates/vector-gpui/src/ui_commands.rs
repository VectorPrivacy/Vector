//! The `/` command picker's wiring: the draft drives it, its keys arrive as `Nav`, and a
//! send asks it where the text goes.

use gpui::prelude::*;
use gpui::{AnyElement, Context, Focusable, SharedString, Window};
use std::rc::Rc;

use super::VectorApp;
use crate::backend;
use crate::commands::{Ctx, Picker, Route, Snapshot};
use crate::composer::Nav;

impl VectorApp {
    pub(super) fn text_effects(&self) -> bool {
        if self.live { vector_core::badges::has_text_effects() } else { std::env::var("VECTOR_EFFECTS").map_or(true, |v| v != "0") }
    }

    /// Runs `f` with the picker and a view of the open chat's commands.
    fn with_commands<R>(&mut self, f: impl FnOnce(&mut Picker, &Ctx) -> R) -> Option<R> {
        let chat = self.open.clone()?;
        let effects = self.text_effects();
        let store = self.store.clone();
        let face = move |npub: &str| {
            let s = store.borrow();
            (SharedString::from(s.name(npub)), s.avatar_path(npub))
        };
        let ctx = Ctx {
            chat: &chat,
            snapshot: self.cmd_snaps.get(chat.as_str()),
            loading: self.cmd_loading.contains(chat.as_str()),
            effects,
            bot_name: &face,
        };
        Some(f(&mut self.commands, &ctx))
    }

    /// The draft's `/` state, re-read on every edit. True while the picker shows.
    pub(super) fn update_commands(&mut self, text: &str, cx: &mut Context<Self>) -> bool {
        let colors = self.text_effects().then(|| {
            let name = text.strip_prefix('/').map(|t| t.split(char::is_whitespace).next().unwrap_or(""));
            name.is_some_and(|n| self.command_is_system(n))
        });
        self.input.update(cx, |i, cx| {
            if i.set_colors(colors) {
                cx.notify();
            }
        });
        let was_open = self.commands.view.is_some();
        if text.starts_with('/') {
            self.load_commands(!was_open, cx);
        }
        self.with_commands(|p, ctx| p.on_input(ctx, text));
        let (list, hint) = (self.commands.is_list(), self.commands.is_hint());
        if list || hint {
            self.shortcode = None;
            self.mention = None;
            self.mention_pool = None;
            self.time_pick = None;
            self.time_chip = None;
            self.input.update(cx, |i, _| {
                i.set_nav(list);
                i.set_chip(hint);
            });
            cx.notify();
            return true;
        }
        if was_open {
            self.input.update(cx, |i, _| {
                i.set_nav(false);
                i.set_chip(false);
            });
            cx.notify();
        }
        false
    }

    /// Asks for the open chat's bot commands; `force` re-asks, so a bot that joined is seen.
    pub(super) fn load_commands(&mut self, force: bool, cx: &mut Context<Self>) {
        let Some(chat) = self.open.clone() else { return };
        if self.cmd_loading.contains(chat.as_str()) || (!force && self.cmd_snaps.contains_key(chat.as_str())) {
            return;
        }
        if !self.live {
            let fixture = self.store.borrow().fixture_bot_commands.as_ref().and_then(|v| v.get(chat.as_ref()).cloned());
            let bots: Vec<(String, Vec<vector_core::bot_interface::CommandSpec>)> = fixture
                .and_then(|v| v.as_array().cloned())
                .unwrap_or_default()
                .iter()
                .filter_map(|b| Some((b.get("bot")?.as_str()?.to_string(), serde_json::from_value(b.get("commands")?.clone()).ok()?)))
                .collect();
            let commands = bots.iter().flat_map(|(bot, c)| crate::commands::bot_commands(bot, c)).collect();
            self.cmd_snaps.insert(chat.to_string(), Snapshot { bots: bots.len(), commands, fresh: true });
            return;
        }
        self.cmd_loading.insert(chat.to_string());
        let rx = backend::run(backend::messaging::chat_commands(chat.to_string()));
        cx.spawn(async move |this, cx| {
            let snap = rx.await.unwrap_or_default();
            this.update(cx, |app, cx| {
                app.cmd_loading.remove(chat.as_str());
                app.cmd_snaps.insert(chat.to_string(), snap);
                if app.open.as_deref() == Some(chat.as_ref()) {
                    app.refresh_rows(false);
                    cx.notify();
                }
                if app.open.as_deref() == Some(chat.as_ref()) && app.commands.view.is_some() {
                    let text = app.input.read(cx).text().to_string();
                    app.update_commands(&text, cx);
                }
            })
            .ok();
        })
        .detach();
    }

    /// The picker's keys. False when it isn't showing.
    pub(super) fn command_nav(&mut self, nav: Nav, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let text = self.input.read(cx).text().to_string();
        if self.commands.is_hint() {
            match nav {
                Nav::Pick => {
                    if let Some(v) = self.commands.first_choice() {
                        self.choose_command(v, window, cx);
                    }
                }
                Nav::Close => {
                    self.commands.suppress(&text);
                    self.input.update(cx, |i, _| i.set_chip(false));
                    cx.notify();
                }
                _ => {}
            }
            return true;
        }
        if !self.commands.is_list() {
            return false;
        }
        match nav {
            Nav::Up => {
                self.with_commands(|p, ctx| p.move_active(ctx, &text, -1));
            }
            Nav::Down => {
                self.with_commands(|p, ctx| p.move_active(ctx, &text, 1));
            }
            Nav::Pick => {
                let i = self.commands.active;
                self.pick_command(i, window, cx);
            }
            Nav::Close => {
                self.commands.close();
                self.input.update(cx, |i, _| i.set_nav(false));
            }
        }
        cx.notify();
        true
    }

    /// The attach panel's Commands: the draft becomes `/`, which opens the list.
    pub(super) fn open_commands(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.attach = None;
        self.input.update(cx, |input, cx| {
            let len = input.text().len();
            input.replace(0..len, "/", window, cx);
        });
        window.focus(&self.input.read(cx).focus_handle(cx), cx);
        let draft = self.input.read(cx).text().to_string();
        self.update_commands(&draft, cx);
    }

    /// An invocation line's command name: the draft becomes `/name`, as if typed.
    pub(super) fn rerun_command(&mut self, name: String, window: &mut Window, cx: &mut Context<Self>) {
        self.input.update(cx, |input, cx| {
            let len = input.text().len();
            input.replace(0..len, &format!("/{name}"), window, cx);
        });
        window.focus(&self.input.read(cx).focus_handle(cx), cx);
        let draft = self.input.read(cx).text().to_string();
        self.update_commands(&draft, cx);
    }

    /// The background manifest refresh landed: swap the chat's list in.
    pub(super) fn on_commands_updated(&mut self, payload: &serde_json::Value, cx: &mut Context<Self>) {
        let Some(chat) = payload.get("chat_id").and_then(|v| v.as_str()) else { return };
        let bots = payload.get("bots").and_then(|v| v.as_u64()).unwrap_or(0) as usize;
        let commands = payload
            .get("commands")
            .and_then(|v| v.as_array())
            .into_iter()
            .flatten()
            .filter_map(|b| {
                let bot = b.get("bot")?.as_str()?;
                let specs: Vec<vector_core::bot_interface::CommandSpec> = serde_json::from_value(b.get("commands")?.clone()).ok()?;
                Some(crate::commands::bot_commands(bot, &specs))
            })
            .flatten()
            .collect();
        self.cmd_snaps.insert(chat.to_string(), Snapshot { bots, commands, fresh: true });
        if self.open.as_deref() == Some(chat) {
            self.refresh_rows(false);
            if self.commands.view.is_some() {
                let text = self.input.read(cx).text().to_string();
                self.update_commands(&text, cx);
            }
            cx.notify();
        }
    }

    /// The command names the open chat's bots declare, for invocation lines.
    pub(super) fn command_names(&self) -> Option<std::rc::Rc<std::collections::HashSet<String>>> {
        let snap = self.cmd_snaps.get(self.open.as_deref()?)?;
        Some(std::rc::Rc::new(snap.commands.iter().map(|c| c.name.clone()).collect()))
    }

    pub(super) fn pick_command(&mut self, i: usize, window: &mut Window, cx: &mut Context<Self>) {
        let (Some(cmd), Some(chat)) = (self.commands.row(i), self.open.clone()) else { return };
        let (text, send) = self.commands.select(&chat, &cmd);
        if !cmd.is_system() && !send {
            self.input.update(cx, |input, cx| {
                input.set_nav(false);
                input.clear(window, cx);
            });
            return self.open_command_form(cmd, chat.to_string(), window, cx);
        }
        self.input.update(cx, |input, cx| {
            input.set_nav(false);
            let len = input.text().len();
            input.replace(0..len, if send { "" } else { &text }, window, cx);
        });
        window.focus(&self.input.read(cx).focus_handle(cx), cx);
        if send {
            self.send(text, cx);
        } else {
            let draft = self.input.read(cx).text().to_string();
            self.update_commands(&draft, cx);
        }
        cx.notify();
    }

    pub(super) fn choose_command(&mut self, value: String, window: &mut Window, cx: &mut Context<Self>) {
        let text = self.input.read(cx).text().to_string();
        let next = Picker::insert_choice(&text, &value);
        self.input.update(cx, |input, cx| {
            let len = input.text().len();
            input.replace(0..len, &next, window, cx);
        });
        window.focus(&self.input.read(cx).focus_handle(cx), cx);
        let draft = self.input.read(cx).text().to_string();
        self.update_commands(&draft, cx);
    }

    /// Where a send goes: `None` refuses it (the reason is shown, the draft kept).
    pub(super) fn route_command(&mut self, text: &str, cx: &mut Context<Self>) -> Option<(String, Option<String>)> {
        let route = self.with_commands(|p, ctx| p.route(ctx, text)).unwrap_or(Route::Plain);
        self.input.update(cx, |i, _| {
            i.set_nav(false);
            i.set_chip(false);
        });
        match route {
            Route::Plain => Some((text.to_string(), None)),
            Route::Text(t) => Some((t, None)),
            Route::Bot(bot) => Some((text.to_string(), Some(bot))),
            Route::Error(e) => {
                self.show_toast(&e, cx);
                None
            }
        }
    }

    /// Whether the open chat's `/name` is Vector's own, so the composer previews its colour.
    pub(super) fn command_is_system(&mut self, name: &str) -> bool {
        self.with_commands(|p, ctx| p.system_owns(ctx, name)).unwrap_or(true)
    }

    /// Bot commands with arguments fill them in pills, one per argument.
    fn open_command_form(&mut self, cmd: crate::commands::Cmd, chat: String, window: &mut Window, cx: &mut Context<Self>) {
        let members = self.command_members();
        let form = cx.new(|cx| crate::cmdform::CommandForm::new(cmd, chat, members, window, cx));
        cx.subscribe_in(&form, window, |app, _, e: &crate::cmdform::Event, window, cx| match e {
            crate::cmdform::Event::Exit => app.exit_command_form(false, window, cx),
            crate::cmdform::Event::Submit(text) => {
                let text = text.clone();
                app.exit_command_form(true, window, cx);
                app.send(text, cx);
            }
        })
        .detach();
        self.attach = None;
        self.picker_open = false;
        self.cmd_form = Some(form.clone());
        form.update(cx, |f, cx| f.focus(0, false, window, cx));
        cx.notify();
    }

    /// Closes the pills; `keep_pick` leaves the command armed so its send routes to its bot.
    pub(super) fn exit_command_form(&mut self, keep_pick: bool, window: &mut Window, cx: &mut Context<Self>) {
        if self.cmd_form.take().is_none() {
            return;
        }
        if !keep_pick {
            self.commands.armed = None;
        }
        window.focus(&self.input.read(cx).focus_handle(cx), cx);
        cx.notify();
    }

    /// Who a user argument can name: the chat's members, as `@` offers them.
    fn command_members(&self) -> Vec<crate::cmdform::Member> {
        let Some(chat_id) = self.open.as_deref() else { return Vec::new() };
        let store = self.store.borrow();
        let Some(chat) = store.chat(chat_id) else { return Vec::new() };
        let roster = self.community.as_ref().map(|v| v.members.as_slice()).unwrap_or_default();
        crate::mentions::candidates(&store, chat, roster, None)
            .into_iter()
            .filter_map(|i| match i.kind {
                crate::mentions::Kind::Member { avatar } => Some(crate::cmdform::Member { npub: i.npub, name: i.name, avatar }),
                _ => None,
            })
            .collect()
    }

    pub(super) fn command_form_view(&self, cx: &mut Context<Self>) -> Option<(AnyElement, AnyElement)> {
        let form = self.cmd_form.clone()?;
        let bot = form.read(cx).cmd.bot.clone();
        let face = {
            let s = self.store.borrow();
            (SharedString::from(s.name(&bot)), s.avatar_path(&bot))
        };
        let app = cx.entity().downgrade();
        let strip = crate::cmdform::strip(form.read(cx), face, move |w, cx| {
            app.update(cx, |app, cx| app.exit_command_form(false, w, cx)).ok();
        });
        Some((strip, form.into_any_element()))
    }

    pub(super) fn command_popup(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let store = self.store.clone();
        let face = move |npub: &str| {
            let s = store.borrow();
            (SharedString::from(s.name(npub)), s.avatar_path(npub))
        };
        let app = cx.entity().downgrade();
        let app2 = app.clone();
        crate::commands::view(
            &self.commands,
            &face,
            Rc::new(move |i, w, cx| {
                app.update(cx, |app, cx| app.pick_command(i, w, cx)).ok();
            }),
            Rc::new(move |v, w, cx| {
                app2.update(cx, |app, cx| app.choose_command(v, w, cx)).ok();
            }),
            self.vw,
        )
    }
}
