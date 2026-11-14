//! `confirmSecretSend`: the guard opens over the composer, which keeps the draft until it's
//! sent or edited.

use gpui::{Context, Window};
use vector_core::secrets::SecretKind;

use super::VectorApp;
use crate::secretguard::{Answer, Guard, Timer};

impl VectorApp {
    pub(super) fn open_secret_guard(&mut self, kind: SecretKind, text: String, chat: String, cx: &mut Context<Self>) {
        self.input.update(cx, |i, cx| i.set_text(&text, cx));
        let (community, place) = {
            let store = self.store.borrow();
            match store.community_of(&chat) {
                Some(com) => (true, com.channel(&chat).map(|ch| ch.name.to_string()).filter(|n| !n.is_empty()).unwrap_or_else(|| "this community".into())),
                None => (false, store.name(&chat)),
            }
        };
        let timer = if kind == SecretKind::NostrKey {
            Timer::None
        } else if self.self_destruct.is_some() {
            Timer::On
        } else {
            Timer::Offer
        };
        self.secret_guard = Some(Guard { kind, text, chat, community, place, timer, opened: std::time::Instant::now() });
        // The Send Anyway countdown.
        cx.spawn(async move |this, cx| {
            for _ in 0..11 {
                cx.background_executor().timer(std::time::Duration::from_secs(1)).await;
                let open = this
                    .update(cx, |this, cx| {
                        cx.notify();
                        this.secret_guard.is_some()
                    })
                    .unwrap_or(false);
                if !open {
                    return;
                }
            }
        })
        .detach();
        cx.notify();
    }

    pub(super) fn secret_answer(&mut self, answer: Answer, _window: &mut Window, cx: &mut Context<Self>) {
        let Some(g) = self.secret_guard.take() else { return };
        match answer {
            Answer::Keep => {}
            Answer::Send if g.wait() == 0 && self.open.as_deref() == Some(g.chat.as_str()) => {
                self.input.update(cx, |i, cx| i.set_text("", cx));
                self.secret_pass = true;
                self.send(g.text, cx);
            }
            Answer::Send => {}
            Answer::Timer => {
                let at = self.chat_menu_at();
                self.open_self_destruct(g.chat, at);
                self.show_toast("Pick a timer, then send it again", cx);
            }
        }
        cx.notify();
    }
}
