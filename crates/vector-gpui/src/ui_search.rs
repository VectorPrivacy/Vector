//! Message search's lifecycle: opened from the keyboard or a chat header, the corpus read in
//! the background, each keystroke a fresh scan, and a result opened where it was said.

use std::rc::Rc;
use std::sync::Arc;

use gpui::{AnyElement, AppContext as _, Context, Focusable as _, SharedString, Window};

use super::VectorApp;
use crate::backend;
use crate::composer::{CancelInput, InputChanged, Nav, TextInput};
use crate::search::{Act, Directory, Search, Suggest};
use crate::t;

impl VectorApp {
    /// Opens search over every chat, or over the open one when `here`.
    pub(super) fn open_search(&mut self, here: bool, window: &mut Window, cx: &mut Context<Self>) {
        let dir = Directory::read(&self.store.borrow());
        let scope = here
            .then(|| self.open.clone())
            .flatten()
            .and_then(|id| dir.chats.iter().find(|c| c.id == id.as_ref()).cloned());
        if let Some(s) = self.search.as_ref() {
            let focus = s.input.focus_handle(cx);
            window.focus(&focus, cx);
            if scope.is_none() {
                return;
            }
        }
        let placeholder: SharedString = match &scope {
            Some(c) => t::search_placeholder_in(c.name.split(" · ").next().unwrap_or(&c.name).to_string()),
            None => t::search_placeholder(),
        };
        let input = cx.new(|cx| {
            let mut i = TextInput::new(placeholder, cx).with_look(crate::composer::Look {
                size: gpui::px(17.),
                ..crate::kit::field_look()
            });
            i.set_nav(true);
            if let Some(c) = &scope {
                let value = c.name.clone();
                let text = crate::search::completion(&Suggest::Value {
                    key: crate::search::Key::In,
                    value,
                    label: SharedString::default(),
                    npub: None,
                });
                i.set_text(&text, cx);
            }
            i
        });
        cx.subscribe_in(&input, window, |app, _, _: &InputChanged, _, cx| app.search_changed(cx)).detach();
        cx.subscribe_in(&input, window, |app, _, nav: &Nav, window, cx| {
            let Some(s) = app.search.as_mut() else { return };
            match nav {
                Nav::Up => s.step(false),
                Nav::Down => s.step(true),
                Nav::Pick => {
                    let sel = s.sel;
                    let act = if s.showing_suggestions() { Act::Suggest(sel) } else { Act::Pick(sel) };
                    return app.on_search(act, window, cx);
                }
                Nav::Close => return app.on_search(Act::Close, window, cx),
            }
            cx.notify();
        })
        .detach();
        cx.subscribe_in(&input, window, |app, _, _: &CancelInput, window, cx| app.on_search(Act::Close, window, cx)).detach();
        window.focus(&input.focus_handle(cx), cx);
        let reuse = self.search.as_mut().and_then(|s| s.corpus.take());
        self.search = Some(Search::new(input, dir));
        self.close_popups(cx);
        match reuse {
            Some(corpus) => self.search_loaded(corpus, cx),
            None => self.search_load(cx),
        }
        self.search_changed(cx);
    }

    fn search_load(&mut self, cx: &mut Context<Self>) {
        let (names, blocked) = {
            let store = self.store.borrow();
            (crate::search::names(&store), crate::search::blocked(&store))
        };
        if !self.live {
            let corpus = Arc::new(crate::search::fixture_corpus(&self.store.borrow()));
            return self.search_loaded(corpus, cx);
        }
        let rx = backend::run(async move {
            let records = vector_core::search::load()?;
            Ok::<_, String>(Arc::new(crate::search::build(&records, &names, &blocked)))
        });
        cx.spawn(async move |this, cx| {
            let res = rx.await;
            this.update(cx, |this, cx| match res {
                Ok(Ok(corpus)) => this.search_loaded(corpus, cx),
                Ok(Err(e)) => this.show_toast(&e, cx),
                Err(_) => {}
            })
            .ok();
        })
        .detach();
    }

    fn search_loaded(&mut self, corpus: Arc<vector_core::search::Corpus>, cx: &mut Context<Self>) {
        if let Some(s) = self.search.as_mut() {
            s.corpus = Some(corpus);
        }
        self.search_changed(cx);
    }

    fn search_changed(&mut self, cx: &mut Context<Self>) {
        let now = self.store.borrow().now_ms();
        let Some(s) = self.search.as_mut() else { return };
        let (text, caret) = {
            let i = s.input.read(cx);
            (i.text().to_string(), i.caret())
        };
        s.refresh(&text, caret, now);
        cx.notify();
    }

    pub(super) fn on_search(&mut self, act: Act, window: &mut Window, cx: &mut Context<Self>) {
        let Some(s) = self.search.as_mut() else { return };
        match act {
            Act::Close => {
                // The plaintext corpus goes with the panel.
                self.search = None;
                window.focus(&self.input.read(cx).focus_handle(cx), cx);
            }
            Act::Hover(i) => {
                if s.sel == i {
                    return;
                }
                s.sel = i;
            }
            Act::Sort => {
                s.oldest = !s.oldest;
                return self.search_changed(cx);
            }
            Act::Suggest(i) => {
                let Some(sg) = s.suggest.get(i).cloned() else { return };
                let range = s.suggest_at.clone();
                let input = s.input.clone();
                input.update(cx, |inp, cx| inp.replace(range, &crate::search::completion(&sg), window, cx));
                return self.search_changed(cx);
            }
            Act::Unfilter(range) => {
                let input = s.input.clone();
                input.update(cx, |inp, cx| {
                    let text = inp.text().to_string();
                    let end = text[range.end..].find(|c: char| !c.is_whitespace()).map_or(text.len(), |n| range.end + n);
                    inp.replace(range.start..end, "", window, cx);
                });
                let focus = input.focus_handle(cx);
                window.focus(&focus, cx);
                return self.search_changed(cx);
            }
            Act::Pick(i) => {
                let Some(hit) = s.found.hits.get(i).cloned() else { return };
                self.search = None;
                self.settings = None;
                self.profile = None;
                self.close_nexus(cx);
                self.new_chat = None;
                self.create_community = None;
                if self.is_slim {
                    self.slim.drawer = false;
                }
                self.open_chat(SharedString::from(hit.chat), window, cx);
                self.jump_or_say(hit.id, t::msg_jump_not_local(), cx);
            }
        }
        cx.notify();
    }

    pub(super) fn search_view(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Option<AnyElement> {
        let s = self.search.as_ref()?;
        let app = cx.entity().downgrade();
        let on: crate::search::OnAct = Rc::new(move |act, window, cx| {
            app.update(cx, |app, cx| app.on_search(act, window, cx)).ok();
        });
        let vp = window.viewport_size();
        let store = self.store.borrow();
        Some(crate::search::view(s, &store, (f32::from(vp.width), f32::from(vp.height)), self.is_slim, cx, &on))
    }
}
