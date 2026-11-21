//! `SecretGuard.svelte`: a draft holding a private key, a seed phrase or a wallet key waits
//! for its sender. Keeping it is the easy answer; Send Anyway unlocks after ten seconds.

use std::rc::Rc;
use std::time::Instant;

use gpui::prelude::*;
use gpui::{AnyElement, App, SharedString, Window, div, px};
use vector_core::secrets::SecretKind;

use crate::security::{actions, button, card, head_tinted, overlay_dismiss};
use crate::t;
use crate::theme::*;

const WAIT_SECS: u64 = 10;
const RED: u32 = 0xff5d6c;

#[derive(Clone, Copy, PartialEq)]
pub enum Timer {
    None,
    Offer,
    On,
}

pub struct Guard {
    pub kind: SecretKind,
    pub text: String,
    pub chat: String,
    pub community: bool,
    pub place: String,
    pub timer: Timer,
    pub opened: Instant,
}

impl Guard {
    pub fn wait(&self) -> u64 {
        WAIT_SECS.saturating_sub(self.opened.elapsed().as_secs())
    }
}

#[derive(Clone, Copy)]
pub enum Answer {
    Keep,
    Send,
    Timer,
}

pub type OnAnswer = Rc<dyn Fn(Answer, &mut Window, &mut App)>;

/// A lead whose `**`-marked runs are `<b>`: bold and white.
fn lead(text: &str, color: gpui::Hsla) -> gpui::Div {
    let mut plain = String::new();
    let mut runs = Vec::new();
    for (i, part) in text.split("**").enumerate() {
        let start = plain.len();
        plain.push_str(part);
        if i % 2 == 1 {
            runs.push((start..plain.len(), gpui::HighlightStyle { color: Some(c(0xffffff)), font_weight: Some(gpui::FontWeight::BOLD), ..Default::default() }));
        }
    }
    div()
        .text_size(px(13.5))
        .line_height(px(20.25))
        .text_color(color)
        .child(gpui::StyledText::new(plain).with_highlights(runs))
}

pub fn render(g: &Guard, on: &OnAnswer) -> AnyElement {
    let key = g.kind == SecretKind::NostrKey;
    let title = match g.kind {
        SecretKind::NostrKey => t::secret_key_title(),
        SecretKind::SeedPhrase => t::secret_seed_title(),
        SecretKind::WalletKey => t::secret_wallet_title(),
    };
    let first = match g.kind {
        SecretKind::NostrKey => t::secret_key_text(),
        SecretKind::SeedPhrase => t::secret_seed_text(),
        SecretKind::WalletKey => t::secret_wallet_text(),
    };
    let place = &*g.place;
    let loud = if g.community {
        t::secret_community(place)
    } else if key {
        t::secret_key_scam(place)
    } else {
        t::secret_wallet_scam(place)
    };
    let timer = match g.timer {
        Timer::Offer => Some(t::secret_timer_offer()),
        Timer::On => Some(t::secret_timer_on()),
        Timer::None => None,
    };
    let answer = |a: Answer| {
        let on = on.clone();
        move |_: &gpui::ClickEvent, w: &mut Window, cx: &mut App| on(a, w, cx)
    };
    let wait = g.wait();
    let gray = white_a(0.62);
    let mut buttons = actions().child(button("secret-keep", t::secret_keep(), false, true, answer(Answer::Keep)));
    if g.timer == Timer::Offer {
        buttons = buttons.child(button("secret-timer", t::secret_set_timer(), true, true, answer(Answer::Timer)));
    }
    let label: SharedString = if wait > 0 { t::secret_send_wait(wait) } else { t::secret_send() };
    buttons = buttons.child(
        div()
            .id("secret-send")
            .w_full()
            .h(px(44.))
            .flex()
            .items_center()
            .justify_center()
            .rounded(px(12.))
            .bg(c(0x1b1c1d))
            .text_color(c(RED))
            .text_size(px(16.))
            .font_weight(gpui::FontWeight::SEMIBOLD)
            .when(wait > 0, |d| d.opacity(0.5))
            .when(wait == 0, |d| d.cursor_pointer().hover(|d| d.bg(ca(255, 93, 108, 0.12))).on_click(answer(Answer::Send)))
            .child(label),
    );
    let close = answer(Answer::Keep);
    overlay_dismiss(
        "secret-guard",
        20,
        answer(Answer::Keep),
        card("secret-card")
            .child(head_tinted(
                "icons/vector_warning.svg",
                title.into(),
                c(RED),
                ca(255, 93, 108, 0.12),
                c(RED),
                Some(Box::new(close)),
            ))
            .child(lead(&first, gray))
            .child(lead(&loud, c(0xffb3ba)))
            .children(timer.map(|t| lead(&t, gray)))
            .child(buttons),
    )
}
