//! `SecretGuard.svelte`: a draft holding a private key, a seed phrase or a wallet key waits
//! for its sender. Keeping it is the easy answer; Send Anyway unlocks after ten seconds.

use std::rc::Rc;
use std::time::Instant;

use gpui::prelude::*;
use gpui::{AnyElement, App, SharedString, Window, div, px};
use vector_core::secrets::SecretKind;

use crate::security::{actions, button, card, head_tinted, overlay_dismiss};
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
        SecretKind::NostrKey => "This is a private key",
        SecretKind::SeedPhrase => "This is a seed phrase",
        SecretKind::WalletKey => "This is a wallet key",
    };
    let what = if g.kind == SecretKind::SeedPhrase { "seed phrase" } else { "wallet private key" };
    let first = if key {
        "Your message contains an **nsec**: the private key to a Nostr account. Whoever has it can read every private message, post as that account and take it over, for good. Once sent, it can't be taken back.".to_string()
    } else {
        format!("Your message looks like a **{what}**. Whoever has it can take everything in that wallet, instantly and for good.")
    };
    let place = &g.place;
    let loud = if g.community {
        format!("This is a community. Everyone in **{place}** would see it, and so would anyone who joins later.")
    } else if key {
        format!("No one ever needs your private key: not Vector, not support, not a friend. If **{place}** asked you for it, it's almost certainly a scam.")
    } else {
        format!("Scammers pose as support staff, giveaways and friends to ask for these. If **{place}** asked, stop and check who they really are.")
    };
    let timer = match g.timer {
        Timer::Offer => Some("If you're sure, turn on a **Self-Destruct Timer** first, so it doesn't stay on devices and relays forever."),
        Timer::On => Some("This chat's Self-Destruct Timer is on, so it won't stay on devices and relays."),
        Timer::None => None,
    };
    let answer = |a: Answer| {
        let on = on.clone();
        move |_: &gpui::ClickEvent, w: &mut Window, cx: &mut App| on(a, w, cx)
    };
    let wait = g.wait();
    let gray = white_a(0.62);
    let mut buttons = actions().child(button("secret-keep", "Don't Send", false, true, answer(Answer::Keep)));
    if g.timer == Timer::Offer {
        buttons = buttons.child(button("secret-timer", "Set a Self-Destruct Timer", true, true, answer(Answer::Timer)));
    }
    let label: SharedString = if wait > 0 { format!("Send Anyway ({wait})").into() } else { "Send Anyway".into() };
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
            .children(timer.map(|t| lead(t, gray)))
            .child(buttons),
    )
}
