//! The `/` command picker's wiring: the draft drives it, its keys arrive as `Nav`, and a
//! send asks it where the text goes.

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
    fn load_commands(&mut self, force: bool, cx: &mut Context<Self>) {
        let Some(chat) = self.open.clone() else { return };
        if self.cmd_loading.contains(chat.as_str()) || (!force && self.cmd_snaps.contains_key(chat.as_str())) {
            return;
        }
        if !self.live {
            self.cmd_snaps.insert(chat.to_string(), Snapshot { fresh: true, ..Snapshot::default() });
            return;
        }
        self.cmd_loading.insert(chat.to_string());
        let rx = backend::run(backend::messaging::chat_commands(chat.to_string()));
        cx.spawn(async move |this, cx| {
            let snap = rx.await.unwrap_or_default();
            this.update(cx, |app, cx| {
                app.cmd_loading.remove(chat.as_str());
                app.cmd_snaps.insert(chat.to_string(), snap);
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

    pub(super) fn pick_command(&mut self, i: usize, window: &mut Window, cx: &mut Context<Self>) {
        let (Some(cmd), Some(chat)) = (self.commands.row(i), self.open.clone()) else { return };
        let (text, send) = self.commands.select(&chat, &cmd);
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
        )
    }
}
