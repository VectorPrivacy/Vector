//! `NetInfo.svelte` in `NetInfoModal`: what Tor or I2P does for you, the login screen's network
//! card. In Settings it drops the line pointing at Settings.

use std::rc::Rc;

use gpui::prelude::*;
use gpui::{AnyElement, App, FontWeight, HighlightStyle, StyledText, Window, div, px, svg};
use crate::pic::img;

use crate::theme::*;

pub const LINKS: &[(&str, &str)] = &[
    ("torAttribution", "https://torproject.org"),
    ("i2p", "https://i2p.net"),
    ("bridges", "https://bridges.torproject.org/bridges/en?transport=obfs4"),
];

/// What the login screen adds: a tab per network, and its I2P router setup under the points.
#[derive(Default)]
pub struct Extras {
    pub tabs: Vec<&'static str>,
    pub on_tab: Option<Rc<dyn Fn(&'static str, &mut Window, &mut App)>>,
    pub router: Option<AnyElement>,
    /// A window of 480px or less: `.lg-net-card`'s slimmer gutters.
    pub narrow: bool,
}

fn tab_bar(tabs: &[&'static str], on: &'static str, pick: Rc<dyn Fn(&'static str, &mut Window, &mut App)>) -> AnyElement {
    let mut bar = div().mt(px(-6.)).mb(px(20.)).p(px(3.)).flex().gap(px(4.)).border_1().border_color(c(0x262626)).rounded(px(9.)).bg(c(0x050505));
    for &k in tabs {
        let pick = pick.clone();
        let lit = k == on;
        bar = bar.child(
            div()
                .id(k)
                .px(px(20.))
                .py(px(5.))
                .rounded(px(6.))
                .text_size(px(13.))
                .line_height(px(16.))
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(c(if lit { 0xffffff } else { 0x8a8a8a }))
                .when(lit, |d| d.bg(c(0x1c1c1c)))
                .hover(|d| d.text_color(c(0xffffff)))
                .cursor_pointer()
                .on_click(move |_, w, cx| pick(k, w, cx))
                .child(if k == "i2p" { "I2P" } else { "Tor" }),
        );
    }
    bar.into_any_element()
}

/// `lead` bold and white, then the rest in the paragraph's own colour.
fn lead_bold(bold: &str, rest: &str, weight: FontWeight) -> StyledText {
    let text = format!("{bold}{rest}");
    StyledText::new(text).with_highlights([(0..bold.len(), HighlightStyle { color: Some(c(0xffffff)), font_weight: Some(weight), ..Default::default() })])
}

pub fn modal(
    kind: &'static str,
    in_settings: bool,
    extras: Extras,
    close: impl Fn(&mut Window, &mut App) + 'static + Clone,
    link: impl Fn(&'static str, &mut Window, &mut App) + 'static,
) -> AnyElement {
    let i2p = kind == "i2p";
    let accent = if i2p { c(0x5fd4ea) } else { c(0xbe5aed) };
    let (title, lead, points, note_b, note, disclaimer, learn, key): (_, _, [(&str, &str); 4], _, _, _, _, &'static str) = if i2p {
        (
            "I2P Network",
            "Route Vector through your own I2P router so relays and servers never see your IP address.",
            [
                ("IP Obfuscation", ": relays see I2P, not you"),
                ("No Exit Needed", ": .i2p relays never leave the network"),
                ("No Central Directory", ": nothing central to block or seize"),
                ("One-Way Tunnels", ": sending and receiving take separate paths"),
            ],
            "Expect slower connections.",
            " Starting can take a few minutes. Servers outside I2P are reached through an outproxy, which can see and link the ones you use.",
            "I2P is independent software run by its volunteers, not operated by Vector.",
            "Learn more about I2P",
            "i2p",
        )
    } else {
        (
            "Tor Network",
            "Route Vector’s connection through Tor so relays and servers never see your real IP address.",
            [
                ("IP Obfuscation", ": relays see Tor, not you"),
                ("Location Privacy", ": your country stays hidden"),
                ("ISP Shielding", ": your provider can’t see which relays you use"),
                ("Censorship Resistance", ": reach relays blocked on your network"),
            ],
            "Expect slower connections.",
            " Your traffic takes a longer path through volunteer relays worldwide so messages and media take more time to send and load, noticeably slower than a VPN.",
            "Tor is independent software maintained by the Tor Project, not operated by Vector.",
            "Learn more about Tor",
            "torAttribution",
        )
    };
    let mut card = div()
        .id("net-info-card")
        .occlude()
        .relative()
        .w(px(529.))
        .max_w_full()
        .flex()
        .flex_col()
        .items_center()
        .map(|d| if extras.narrow { d.pt(px(30.)).px(px(20.)).pb(px(22.)) } else { d.pt(px(37.)).px(px(34.)).pb(px(26.)) })
        .border_1()
        .border_color(c(0x474747))
        .rounded(px(9.))
        .bg(c(0x0a0a0a))
        .text_color(c(0xffffff))
        .text_center()
        .on_click(|_, _, cx| cx.stop_propagation())
        .when(extras.tabs.len() > 1, |d| d.h(px(640.)).max_h_full().overflow_y_scroll())
        .children(match (extras.tabs.len() > 1, extras.on_tab) {
            (true, Some(pick)) => Some(tab_bar(&extras.tabs, kind, pick)),
            _ => None,
        })
        .child({
            let close = close.clone();
            div()
                .id("net-info-close")
                .absolute()
                .top(px(14.))
                .right(px(16.))
                .p(px(4.))
                .text_size(px(16.))
                .line_height(px(16.))
                .text_color(c(ICON_MUTED))
                .hover(|d| d.text_color(c(0xffffff)))
                .cursor_pointer()
                .on_click(move |_, w, cx| close(w, cx))
                .child("✕")
        })
        .child(if i2p {
            crate::netglyph::glyph(crate::netglyph::Net::I2p, crate::netglyph::State::Connected, None, 56.)
        } else {
            img("icons/tor-logo.svg").w(px(119.)).h(px(72.)).into_any_element()
        })
        .child(div().mt(px(12.)).text_size(px(22.)).line_height(px(27.)).font_weight(FontWeight::BOLD).child(title))
        .child(div().mt(px(12.)).max_w(px(400.)).text_size(px(15.)).line_height(px(20.)).text_color(accent).child(lead));
    let mut list = div().mt(px(if i2p { 14. } else { 20. })).py(px(4.)).gap(px(4.)).flex().flex_col().text_size(px(14.)).line_height(px(20.)).text_color(c(ICON_MUTED));
    for (b, rest) in points {
        list = list.child(
            div()
                .relative()
                .pl(px(16.))
                .child(div().absolute().left(px(2.)).text_color(c(0xffffff)).child("•"))
                .child(lead_bold(b, rest, FontWeight::BOLD)),
        );
    }
    card = card.child(div().flex().justify_center().child(list.text_left())).children(extras.router);
    card = card.child(
        div()
            .w_full()
            .mt(px(if i2p { 16. } else { 22. }))
            .py(px(10.))
            .px(px(14.))
            .rounded(px(8.))
            .bg(c(CHAT_BG))
            .text_size(px(13.))
            .line_height(px(17.))
            .text_color(c(ICON_MUTED))
            .text_left()
            .child(lead_bold(note_b, note, FontWeight::MEDIUM)),
    );
    if !in_settings {
        card = card.child(
            div()
                .mt(px(14.))
                .text_size(px(12.))
                .line_height(px(15.))
                .text_color(c(ICON_MUTED))
                .child(lead_bold("", "You can change this any time in Settings > Privacy > Routing.", FontWeight::NORMAL)),
        );
    }
    card = card
        .child(div().mt(px(if i2p { 18. } else { 26. })).text_size(px(11.)).line_height(px(14.)).italic().text_color(c(0x5c5c5c)).child(disclaimer))
        .child(
            div()
                .id("net-info-learn")
                .mt(px(14.))
                .flex()
                .items_center()
                .gap(px(5.))
                .text_size(px(12.))
                .line_height(px(15.))
                .text_color(accent)
                .cursor_pointer()
                .hover(|d| d.underline())
                .on_click(move |_, w, cx| link(key, w, cx))
                .child(learn)
                .child(svg().path("ui/external-link.svg").size(px(13.)).text_color(accent)),
        );
    div()
        .id("net-info")
        .absolute()
        .inset_0()
        .occlude()
        .flex()
        .items_center()
        .justify_center()
        .p(px(16.))
        .pt(px(16. + 31.))
        .bg(ca(0, 0, 0, 0.72))
        .on_click(move |_, w, cx| close(w, cx))
        .child(card)
        .into_any_element()
}
