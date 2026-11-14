//! How each relay and server is reached on the network in use (`routeOf`, `transportRouteTag`,
//! `transportRouteBadge`), the Connection block of their dialogs, and the address a server keeps
//! inside Tor or I2P (`AliasField.svelte`).

use std::collections::HashMap;

use gpui::prelude::*;
use gpui::{AnyElement, App, Context, Entity, FontWeight, SharedString, Window, div, px, svg};
use serde_json::Value;

use crate::composer::{Look, TextInput};
use crate::markdown::MONO;
use crate::theme::*;

const I2P_ONLY_REFUSAL: &str = "I2P-Only is on, so this server is off.";

fn s<'a>(v: &'a Value, k: &str) -> &'a str {
    v.get(k).and_then(Value::as_str).unwrap_or("")
}

/// The network's view of every listed server, shared by the list and its dialogs.
#[derive(Clone, Default)]
pub struct RouteBook {
    pub view: Option<Value>,
    /// Lowercased url → RouteView.
    pub routes: HashMap<String, Value>,
    pub aliases: Vec<Value>,
    /// I2P's settings: I2P-Only rules out a lookup over clearnet.
    pub config: Option<Value>,
}

pub struct Tag {
    pub cls: &'static str,
    pub text: String,
    pub short: Option<&'static str>,
    pub title: String,
}

pub struct Badge {
    pub failed: bool,
    pub text: String,
    pub title: String,
}

impl RouteBook {
    pub fn route_of(&self, url: &str) -> Option<&Value> {
        self.routes.get(&url.to_lowercase())
    }

    fn starting(&self) -> bool {
        self.view.as_ref().is_some_and(|v| s(v, "phase") == "starting")
    }

    pub fn alias(&self, host: &str) -> Option<&Value> {
        self.aliases.iter().find(|a| s(a, "host") == host)
    }

    /// The list's word for a route that can't connect on the network in use.
    pub fn tag(&self, url: &str) -> Option<Tag> {
        self.view.as_ref()?;
        let r = self.route_of(url)?;
        let (class, text) = (s(r, "class"), s(r, "text"));
        let title = r.get("last_error").and_then(Value::as_str).unwrap_or(text).to_string();
        if class == "blocked" {
            // A network still starting holds traffic as a matter of course: not an error.
            return Some(if self.starting() {
                Tag { cls: "connecting", text: "Connecting".into(), short: None, title }
            } else {
                Tag { cls: "blocked", text: "Blocked".into(), short: None, title }
            });
        }
        if class != "refused" {
            return None;
        }
        if text == I2P_ONLY_REFUSAL {
            return Some(Tag { cls: "refused", text: "Off in I2P-Only".into(), short: Some("I2P-Only"), title: text.into() });
        }
        let host = s(r, "host").to_ascii_lowercase();
        // A build without Tor reaches no .onion, so only a refusal that names Tor promises it.
        let needs = if host.ends_with(".i2p") {
            "I2P"
        } else if host.ends_with(".onion") && text.ends_with("over Tor.") {
            "Tor"
        } else {
            ""
        };
        let word = if needs.is_empty() { "Unreachable".to_string() } else { format!("Needs {needs}") };
        Some(Tag { cls: "refused", text: word, short: None, title: text.into() })
    }

    /// A mark beside a row that connects: it rides its address inside the network, or the one
    /// saved for it failed its check.
    pub fn badge(&self, url: &str) -> Option<Badge> {
        self.view.as_ref()?;
        let r = self.route_of(url)?;
        let kind = s(r, "kind");
        let noun = if kind == "tor" { "Onion" } else { "I2P" };
        if s(r, "class") == "twin" {
            return Some(Badge { failed: false, text: noun.into(), title: s(r, "text").into() });
        }
        if (kind != "i2p" && kind != "tor") || s(r, "class") != "exit" {
            return None;
        }
        let a = self.alias(s(r, "host"))?;
        if a["twins"][kind].is_null() || a["check"]["state"] != "failed" {
            return None;
        }
        let title = a["check"]["text"].as_str().unwrap_or(s(r, "text")).to_string();
        Some(Badge { failed: true, text: format!("{noun} address failed"), title })
    }

    /// The network an address field is offered for: the one in use, when it has addresses.
    pub fn alias_kind(&self) -> Option<&'static str> {
        match self.view.as_ref().map(|v| s(v, "kind")) {
            Some("tor") => Some("tor"),
            Some("i2p") => Some("i2p"),
            _ => None,
        }
    }
}

/// `.st-page .relay-status.{refused,blocked,connecting}` in the list, icon and words.
/// `short`: a phone row's form, `.tag-short` in place of `.tag-long`.
pub fn tag_status(t: &Tag, short: bool) -> AnyElement {
    let (color, icon) = match t.cls {
        "refused" => (c(0x8fd8e8), "icons/shield-filled.svg"),
        "blocked" => (c(crate::timeline::DANGER), "icons/cancel.svg"),
        _ => (c(0xffc107), "icons/clock.svg"),
    };
    div()
        .id(SharedString::from(format!("tag-{}", t.title)))
        .flex()
        .flex_shrink_0()
        .items_center()
        .gap(px(6.))
        .text_size(px(16.))
        .line_height(px(19.))
        .text_color(color)
        .tooltip(crate::kit::tooltip(t.title.clone()))
        .child(svg().path(icon).size(px(16.)).flex_shrink_0().text_color(color))
        .child(match t.short.filter(|_| short) {
            Some(s) => SharedString::from(s),
            None => SharedString::from(t.text.clone()),
        })
        .into_any_element()
}

/// `.relay-route-badge`: a twin rides inside the network; a dashed one failed its check.
/// `dot`: a phone row's 8px dot, its words left to the tooltip.
pub fn badge(b: &Badge, dot: bool) -> AnyElement {
    if dot {
        return div()
            .id(SharedString::from(format!("badge-{}", b.title)))
            .flex_shrink_0()
            .mr(px(2.))
            .size(px(8.))
            .rounded_full()
            .border_1()
            .when(!b.failed, |d| d.bg(c(0x5fd4ea)).border_color(ca(64, 200, 228, 0.3)))
            .when(b.failed, |d| d.border_dashed().border_color(white_a(0.2)))
            .tooltip(crate::kit::tooltip(b.title.clone()))
            .into_any_element();
    }
    div()
        .id(SharedString::from(format!("badge-{}", b.title)))
        .flex_shrink_0()
        .mr(px(6.))
        .px(px(6.))
        .py(px(1.))
        .rounded(px(6.))
        .border_1()
        .text_size(px(11.))
        .line_height(px(16.))
        .whitespace_nowrap()
        .when(!b.failed, |d| d.text_color(c(0x7adef0)).bg(ca(64, 200, 228, 0.12)).border_color(ca(64, 200, 228, 0.3)))
        .when(b.failed, |d| d.text_color(white_a(0.45)).border_dashed().border_color(white_a(0.2)))
        .tooltip(crate::kit::tooltip(b.title.clone()))
        .child(b.text.clone())
        .into_any_element()
}

/// The dialog header's pill for a held route (`.relay-status-small`).
pub fn tag_colors(cls: &str) -> (gpui::Hsla, gpui::Hsla, gpui::Hsla) {
    match cls {
        "refused" => (c(0x8fd8e8), ca(64, 200, 228, 0.08), ca(64, 200, 228, 0.28)),
        "blocked" => (c(0xff8c75), ca(255, 120, 96, 0.12), ca(255, 120, 96, 0.32)),
        _ => (c(0xffc107), ca(255, 193, 7, 0.2), ca(255, 193, 7, 0.3)),
    }
}

fn section(title: impl IntoElement) -> gpui::Div {
    div().flex().flex_col().pt(px(12.)).border_t_1().border_color(c(0x262626)).child(title)
}

fn h4(text: &'static str) -> gpui::Div {
    div().flex().items_center().text_size(px(13.)).line_height(px(24.)).font_weight(FontWeight::SEMIBOLD).text_color(white_a(0.7)).child(text)
}

/// `ConnectionBlock`: how Vector reaches this server now, a recent failure, and over Tor its circuit.
pub fn connection(book: &RouteBook, url: &str, circuit: Option<&[Value]>) -> Option<AnyElement> {
    let route = book.route_of(url);
    if route.is_none() && circuit.is_none() {
        return None;
    }
    let mut col = section(h4("Connection"));
    if let Some(r) = route {
        let class = s(r, "class");
        let held = class == "blocked" && book.starting();
        let color = if held || class == "connecting" {
            white_a(0.6)
        } else {
            match class {
                "refused" => c(0x8fd8e8),
                "blocked" => c(0xff8c75),
                "direct" => white_a(0.6),
                _ => match s(r, "kind") {
                    "tor" => c(0xd9b3ff),
                    "i2p" => c(0x7adef0),
                    _ => white_a(0.88),
                },
            }
        };
        let text = if held {
            let label = book.view.as_ref().map(|v| s(v, "label")).unwrap_or("");
            if label.is_empty() { "Waiting for the network to connect".to_string() } else { format!("Waiting for {label} to connect") }
        } else {
            s(r, "text").to_string()
        };
        col = col.child(div().mt(px(6.)).text_size(px(13.)).line_height(px(18.2)).text_color(color).child(text));
        if let Some(e) = r.get("last_error").and_then(Value::as_str).filter(|_| class != "blocked" && class != "refused") {
            col = col.child(div().mt(px(4.)).text_size(px(12.)).line_height(px(16.8)).text_color(c(0xff8c75)).child(e.to_string()));
        }
    }
    if let Some(hops) = circuit {
        col = col.child(div().mt(px(8.)).child(crate::routing::hops(hops)));
    }
    Some(col.into_any_element())
}

#[derive(Clone, Copy, PartialEq)]
pub enum Tone {
    Muted,
    Ok,
    Error,
}

/// One dialog's address field: what's typed, and what the last action said.
pub struct Alias {
    pub kind: &'static str,
    pub url: String,
    pub input: Entity<TextInput>,
    pub busy: bool,
    pub note: Option<(String, Tone)>,
    /// Typed (or found) and not yet saved: the saved address no longer overwrites it.
    pub editing: bool,
    pub relay: bool,
}

pub enum AliasAct {
    Help(&'static str),
    Save,
    Find,
    Check,
    Remove,
}

fn look() -> Look {
    Look {
        family: MONO,
        weight: FontWeight::MEDIUM,
        size: px(12.),
        line_height: px(18.),
        color: c(0xffffff),
        placeholder: white_a(0.25),
        ..Default::default()
    }
}

struct Target {
    host: String,
    port: u16,
}

fn target(url: &str) -> Option<Target> {
    let u = url::Url::parse(url).ok()?;
    let host = u.host_str()?.trim_end_matches('.').to_ascii_lowercase();
    Some(Target { host, port: u.port_or_known_default()? })
}

struct Net {
    label: &'static str,
    title: &'static str,
    noun: &'static str,
    suffix: &'static str,
    placeholder: &'static str,
    help: &'static str,
    color: u32,
}

fn net(kind: &str) -> Net {
    if kind == "tor" {
        Net { label: "Tor", title: "Onion Address", noun: "onion address", suffix: ".onion", placeholder: "xxxx.onion", help: "onionAlias", color: 0xd9b3ff }
    } else {
        Net { label: "I2P", title: "I2P Address", noun: "I2P address", suffix: ".i2p", placeholder: "xxxx.b32.i2p", help: "i2pAlias", color: 0x7adef0 }
    }
}

impl Alias {
    pub fn new<V: 'static>(book: &RouteBook, url: &str, relay: bool, cx: &mut Context<V>) -> Option<Self> {
        let kind = book.alias_kind()?;
        let input = cx.new(|cx| TextInput::new(net(kind).placeholder, cx).with_look(look()).keep_on_submit());
        let mut a = Self { kind, url: url.to_string(), input, busy: false, note: None, editing: false, relay };
        a.sync(book, cx);
        Some(a)
    }

    /// The saved address fills the field unless the user is editing it.
    pub fn sync(&mut self, book: &RouteBook, cx: &mut App) {
        if self.editing {
            return;
        }
        let saved = self.saved(book);
        self.input.update(cx, |i, cx| {
            if i.text() != saved {
                i.set_text(&saved, cx);
            }
        });
    }

    pub fn host(&self) -> Option<String> {
        target(&self.url).map(|t| t.host)
    }

    pub fn saved(&self, book: &RouteBook) -> String {
        self.host().and_then(|h| book.alias(&h).and_then(|a| a["twins"][self.kind].as_str()).map(str::to_string)).unwrap_or_default()
    }

    pub fn typed(&self, cx: &App) -> String {
        self.input.read(cx).text().trim().to_ascii_lowercase()
    }

    fn dirty(&self, book: &RouteBook, cx: &App) -> bool {
        self.typed(cx) != self.saved(book)
    }
}

/// `AliasField.svelte`.
pub fn alias_field(a: &Alias, book: &RouteBook, on: impl Fn(AliasAct, &mut Window, &mut App) + Clone + 'static, cx: &App) -> Option<AnyElement> {
    let t = target(&a.url)?;
    let n = net(a.kind);
    let supported = book.view.as_ref().and_then(|v| v.get("supported")).and_then(Value::as_array).is_some_and(|l| l.iter().any(|k| k == a.kind));
    if !supported || t.host.ends_with(n.suffix) {
        return None;
    }
    let entry = book.alias(&t.host);
    let saved = a.saved(book);
    let built_in = entry.is_some_and(|e| e["source"] == "builtin");
    let ready = book.view.as_ref().is_some_and(|v| s(v, "kind") == a.kind && v.get("ready").and_then(Value::as_bool) == Some(true));
    let can_find = a.relay && (a.kind != "i2p" || book.config.as_ref().is_none_or(|c| c["exit"] != "off"));
    let usable = t.port == 443;
    let dirty = a.dirty(book, cx);
    let line: Option<(String, Tone)> = if t.port != 443 {
        Some((
            if saved.is_empty() {
                format!("This server doesn't use port 443, so it can't use an {}.", n.noun)
            } else {
                "This server doesn't use port 443, so this address isn't used.".into()
            },
            Tone::Muted,
        ))
    } else if saved.is_empty() {
        None
    } else {
        let check = entry.map(|e| &e["check"]).cloned().unwrap_or(Value::Null);
        let lead = if built_in { "Built into Vector. " } else { "" };
        let text = check["text"].as_str();
        Some(match check["state"].as_str() {
            Some("ok") => (format!("{lead}{}", text.map_or_else(|| format!("Checked. It serves {}.", t.host), str::to_string)), Tone::Ok),
            Some("failed") => (text.map_or_else(|| format!("This address doesn't serve {}. Vector won't use it.", t.host), str::to_string), Tone::Error),
            _ => (format!("{lead}{}", text.map_or_else(|| format!("Not checked yet. Vector checks it when {} connects.", n.label), str::to_string)), Tone::Muted),
        })
    };
    let say = |(text, tone): (String, Tone)| {
        div()
            .mt(px(8.))
            .text_size(px(12.5))
            .line_height(px(17.5))
            .text_color(match tone {
                Tone::Ok => c(0x7adef0),
                Tone::Error => c(0xff8c75),
                Tone::Muted => white_a(0.5),
            })
            .child(text)
    };
    let act = |a: fn() -> AliasAct| {
        let on = on.clone();
        move |_: &gpui::ClickEvent, w: &mut Window, cx: &mut App| on(a(), w, cx)
    };
    let button = |id: &'static str, text: &'static str, primary: bool, enabled: bool| {
        div()
            .id(id)
            .h(px(38.))
            .px(px(14.))
            .flex()
            .items_center()
            .rounded(px(6.))
            .border_1()
            .border_color(if primary { ca(64, 200, 228, 0.45) } else { c(0x333333) })
            .bg(if primary { ca(64, 200, 228, 0.14) } else { c(0x262626) })
            .text_size(px(12.))
            .text_color(if primary { c(0xc9f2fa) } else { c(0xffffff) })
            .when(!enabled, |d| d.opacity(0.5))
            .when(enabled, |d| d.cursor_pointer().hover(move |d| d.bg(if primary { ca(64, 200, 228, 0.24) } else { c(0x333333) })))
            .child(text)
    };
    let help = {
        let on = on.clone();
        let key = n.help;
        crate::settings::info("alias-help", move |_, w, cx| on(AliasAct::Help(key), w, cx))
    };
    let mut col = section(h4(n.title).child(div().ml(px(6.)).child(help)));
    if usable {
        col = col.child(
            div()
                .mt(px(8.))
                .flex()
                .gap(px(8.))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .h(px(38.))
                        .px(px(12.))
                        .flex()
                        .items_center()
                        .rounded(px(8.))
                        .border_1()
                        .border_color(c(0x262626))
                        .bg(ca(0, 0, 0, 0.3))
                        .when(a.busy, |d| d.opacity(0.5))
                        .overflow_hidden()
                        .child(a.input.clone()),
                )
                .child(button("alias-save", "Save", true, !a.busy && dirty).when(!a.busy && dirty, |d| d.on_click(act(|| AliasAct::Save)))),
        );
    } else if !saved.is_empty() {
        col = col.child(
            div()
                .mt(px(8.))
                .flex()
                .gap(px(8.))
                .child(div().flex_1().min_w_0().truncate().font_family(MONO).text_size(px(12.)).line_height(px(38.)).text_color(white_a(0.45)).child(saved.clone()))
                .child(button("alias-remove-row", "Remove", false, !a.busy).when(!a.busy, |d| d.on_click(act(|| AliasAct::Remove)))),
        );
    }
    if let Some(n) = a.note.clone().filter(|(t, _)| !t.is_empty()) {
        col = col.child(say(n));
    }
    if let Some(l) = line {
        col = col.child(say(l));
    }
    let find = ready && can_find;
    if usable && (find || !saved.is_empty()) {
        let link = |id: &'static str, text: &'static str, f: fn() -> AliasAct| {
            div()
                .id(id)
                .text_size(px(13.))
                .line_height(px(24.))
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(c(n.color))
                .when(a.busy, |d| d.opacity(0.45))
                .when(!a.busy, |d| d.cursor_pointer().hover(|d| d.underline()).on_click(act(f)))
                .child(text)
        };
        col = col.child(
            div()
                .mt(px(8.))
                .flex()
                .gap(px(16.))
                .when(find, |d| d.child(link("alias-find", "Find", || AliasAct::Find)))
                .when(ready && !saved.is_empty(), |d| d.child(link("alias-check", "Check", || AliasAct::Check)))
                .when(!saved.is_empty(), |d| d.child(link("alias-remove", "Remove", || AliasAct::Remove))),
        );
    }
    Some(col.into_any_element())
}
