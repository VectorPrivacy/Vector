//! The Policies tab of the Moderation console (`moderation/Policy*.svelte`): the community's
//! policies and the template gallery, or the editor for one draft. A policy is never enabled
//! from a form alone, only from a preview of that same draft.

use std::collections::{HashMap, HashSet};

use gpui::prelude::*;
use gpui::{AnyElement, App, Entity, FontWeight, SharedString, Window, div, px, svg};
use serde_json::{Value, json};

use crate::composer::TextInput;
use crate::moderation::{Act as ModAct, Moderation, OnAct};
use crate::theme::*;

pub struct Strictness {
    pub id: &'static str,
    pub label: &'static str,
    mult: f64,
    first: u64,
    pub says: &'static str,
}

pub const STRICTNESS: [Strictness; 3] = [
    Strictness {
        id: "relaxed",
        label: "Relaxed",
        mult: 0.8,
        first: 2,
        says: "Two hits in one message before it says anything, with lower confidence.",
    },
    Strictness {
        id: "balanced",
        label: "Balanced",
        mult: 1.0,
        first: 1,
        says: "One hit is enough, at this template's own confidence.",
    },
    Strictness {
        id: "strict",
        label: "Strict",
        mult: 1.1,
        first: 1,
        says: "One hit is enough, with higher confidence.",
    },
];

pub const STRICTNESS_EXPLAINER: &str = "Sensitivity sets how much it takes to trip a rule, and how confident the result is.\n\nRelaxed: two hits in one message, with lower confidence.\n\nBalanced: one hit, at the template's own confidence.\n\nStrict: one hit, with higher confidence.";

pub struct Rule {
    pub kind: String,
    pub input: Entity<TextInput>,
}

pub struct Draft {
    pub id: String,
    pub name: String,
    pub caveat: String,
    pub dials: Vec<Value>,
    pub summary: Vec<Value>,
    pub policy: Value,
    pub strictness: &'static str,
    /// Text dials by key.
    pub inputs: HashMap<String, Entity<TextInput>>,
    pub channels: HashSet<String>,
    pub rules: Vec<Rule>,
    /// The link dial was filled from the rule's own list, so it replaces it on save.
    pub seeded: bool,
}

#[derive(Default)]
pub struct Policies {
    pub loaded: bool,
    pub presets: Vec<Value>,
    pub kinds: Vec<Value>,
    pub stored: Vec<Value>,
    pub using_builtin: bool,
    pub draft: Option<Draft>,
    pub previewed: bool,
    pub busy: bool,
    pub preview: Option<Value>,
    pub preview_error: Option<String>,
}

pub enum Act {
    Inspect,
    Open(String),
    Back,
    Strictness(&'static str),
    Channel(String),
    AddRule(String),
    RemoveRule(usize),
    Preview,
    Save,
    Toggle(String),
    Remove(String),
    Explain,
}

impl Policies {
    /// Any edit invalidates the preview: you cannot preview one policy and enable another.
    pub fn dirty(&mut self) {
        self.previewed = false;
        self.preview_error = None;
        self.preview = None;
    }

    pub fn kind(&self, id: &str) -> Option<&Value> {
        self.kinds.iter().find(|k| k["id"].as_str() == Some(id))
    }
}

fn seconds(value: &str, fallback: u64) -> u64 {
    value
        .trim()
        .parse::<i64>()
        .map(|n| n.clamp(1, 3600) as u64)
        .unwrap_or(fallback)
}

fn split(text: &str) -> Vec<String> {
    text.split(['\n', ','])
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(String::from)
        .collect()
}

/// `polCompose`: the dials folded into the policy document the engine evaluates.
pub fn compose(p: &Policies, cx: &App) -> Option<Value> {
    let d = p.draft.as_ref()?;
    let mut doc = d.policy.clone();
    let s = STRICTNESS
        .iter()
        .find(|s| s.id == d.strictness)
        .unwrap_or(&STRICTNESS[1]);
    let text = |key: &str| {
        d.inputs
            .get(key)
            .map(|i| i.read(cx).text().to_string())
            .unwrap_or_default()
    };
    let name = text("name");
    if !name.is_empty() {
        doc["name"] = name.chars().take(64).collect::<String>().into();
    }
    let per = text("per_secs");
    if !per.is_empty() {
        for rule in doc["rules"].as_array_mut()?.iter_mut() {
            if rule["match"]["type"] == "rate" {
                let fallback = rule["match"]["per_secs"].as_u64().unwrap_or(10);
                rule["match"]["per_secs"] = seconds(&per, fallback).into();
            }
        }
    }
    if d.dials.iter().any(|x| x["kind"] == "rules") {
        let mut seen = HashSet::new();
        let rules: Vec<Value> = d
            .rules
            .iter()
            .enumerate()
            .filter_map(|(i, r)| {
                let kind = p.kind(&r.kind)?;
                let mut built = kind["rule"].clone();
                let base = built["id"].as_str().unwrap_or("rule").to_string();
                let mut id = base.clone();
                while seen.contains(&id) {
                    id = format!("{base}-{i}");
                }
                seen.insert(id.clone());
                built["id"] = id.into();
                let value = r.input.read(cx).text().to_string();
                match kind["input"].as_str() {
                    Some("seconds") => {
                        let fallback = built["match"]["per_secs"].as_u64().unwrap_or(10);
                        built["match"]["per_secs"] = seconds(&value, fallback).into();
                    }
                    Some("none") => {}
                    _ => built["match"]["patterns"] = split(&value).into(),
                }
                Some(built)
            })
            .collect();
        doc["rules"] = rules.into();
    }
    let words = split(&text("words"));
    let domains = split(&text("domains"));
    let allow = split(&text("allow"));
    let chans: Vec<String> = d.channels.iter().cloned().collect();
    for rule in doc["rules"].as_array_mut()?.iter_mut() {
        if let Some(tiers) = rule.get_mut("tiers") {
            for scope in ["per_message", "per_window"] {
                for (i, rung) in tiers[scope]
                    .as_array_mut()
                    .into_iter()
                    .flatten()
                    .enumerate()
                {
                    let w = rung["weight"].as_f64().unwrap_or(1.);
                    rung["weight"] = ((w * s.mult).round().clamp(1., 99.) as u64).into();
                    if i == 0 && scope == "per_message" {
                        rung["hits"] = s.first.into();
                    }
                }
            }
        }
        match rule["match"]["type"].as_str() {
            Some("keyword") if !words.is_empty() => {
                rule["match"]["patterns"] = words.clone().into()
            }
            Some("link") => {
                if d.seeded {
                    rule["match"]["patterns"] = domains.clone().into();
                } else if !domains.is_empty() {
                    let mut all: Vec<String> = rule["match"]["patterns"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .filter_map(|v| v.as_str().map(String::from))
                        .collect();
                    for x in &domains {
                        if !all.contains(x) {
                            all.push(x.clone());
                        }
                    }
                    rule["match"]["patterns"] = all.into();
                }
                if !allow.is_empty() {
                    let mut exempt = rule
                        .get("exempt")
                        .cloned()
                        .filter(Value::is_object)
                        .unwrap_or_else(|| json!({}));
                    exempt["patterns"] = json!([{ "kind": "domain", "values": allow }]);
                    rule["exempt"] = exempt;
                }
            }
            _ => {}
        }
        if !chans.is_empty() {
            let mut exempt = rule
                .get("exempt")
                .cloned()
                .filter(Value::is_object)
                .unwrap_or_else(|| json!({}));
            exempt["channels"] = chans.clone().into();
            rule["exempt"] = exempt;
        }
    }
    Some(doc)
}

/// `polReadiness`: whether the draft is worth running, and why not if it isn't.
pub fn readiness(p: &Policies, cx: &App) -> Result<Value, String> {
    let doc = compose(p, cx).ok_or("This policy is not ready yet.")?;
    let rules = doc["rules"].as_array().cloned().unwrap_or_default();
    if rules.is_empty() {
        return Err("Add at least one rule before previewing.".into());
    }
    let bare = rules.iter().find(|r| {
        matches!(r["match"]["type"].as_str(), Some("keyword" | "link"))
            && r["match"]["patterns"].as_array().is_none_or(Vec::is_empty)
    });
    if let Some(r) = bare {
        return Err(if r["match"]["type"] == "link" {
            "One of your rules has no domains in it."
        } else {
            "One of your rules has no words in it."
        }
        .into());
    }
    Ok(doc)
}

/// A stable, readable id for a policy saved from scratch.
pub fn new_id(name: &str, stored: &[Value]) -> String {
    let mut base = String::new();
    for ch in name.to_lowercase().chars() {
        if ch.is_ascii_alphanumeric() {
            base.push(ch);
        } else if !base.ends_with('-') {
            base.push('-');
        }
    }
    let base: String = base.trim_matches('-').chars().take(40).collect();
    let base = if base.is_empty() {
        "policy".to_string()
    } else {
        base
    };
    let taken: HashSet<&str> = stored
        .iter()
        .filter_map(|p| p["policy_id"].as_str())
        .collect();
    if !taken.contains(base.as_str()) {
        return base;
    }
    (2..500)
        .map(|i| format!("{base}-{i}"))
        .find(|id| !taken.contains(id.as_str()))
        .unwrap_or(base)
}

fn click(
    on: &OnAct,
    act: impl Fn() -> Act + 'static,
) -> impl Fn(&gpui::ClickEvent, &mut Window, &mut App) + 'static {
    let on = on.clone();
    move |_, w, cx| on(ModAct::Policy(act()), w, cx)
}

fn muted() -> gpui::Hsla {
    c(0x8b8b8b)
}

fn list(p: &Policies, on: &OnAct) -> impl IntoElement {
    let row = |builtin: bool| {
        div()
            .flex()
            .items_center()
            .gap(px(12.))
            .px(px(14.))
            .py(px(12.))
            .rounded(px(10.))
            .border_1()
            .when(builtin, |d| {
                d.bg(ca(89, 252, 179, 0.045))
                    .border_color(ca(89, 252, 179, 0.16))
            })
            .when(!builtin, |d| {
                d.bg(white_a(0.04)).border_color(white_a(0.07))
            })
    };
    let name = |t: String| {
        div()
            .font_weight(FontWeight::SEMIBOLD)
            .text_size(px(14.))
            .line_height(px(24.))
            .text_color(c(0xf6f6f6))
            .child(t)
    };
    let sub = |t: String, tone: gpui::Hsla| {
        div()
            .text_size(px(12.))
            .line_height(px(24.))
            .text_color(tone)
            .child(t)
    };
    let builtin = if p.using_builtin {
        row(true)
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .child(name("Vector's defaults".into()))
                    .child(sub("Watching for raid waves · running".into(), muted())),
            )
            .child(
                div()
                    .id("pol-inspect")
                    .px(px(13.))
                    .py(px(6.))
                    .rounded_full()
                    .border_1()
                    .border_color(ca(89, 252, 179, 0.28))
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_size(px(12.))
                    .line_height(px(14.))
                    .text_color(ca(89, 252, 179, 0.85))
                    .cursor_pointer()
                    .hover(|d| {
                        d.bg(ca(89, 252, 179, 0.1))
                            .border_color(ca(89, 252, 179, 0.5))
                    })
                    .on_click(click(on, || Act::Inspect))
                    .child("See the rules"),
            )
    } else {
        row(false).bg(white_a(0.03)).child(
            div()
                .flex_1()
                .min_w_0()
                .child(name("Vector's defaults".into()))
                .child(sub(
                    "Replaced by your own version below.".into(),
                    white_a(0.4),
                )),
        )
    };
    div()
        .flex()
        .flex_col()
        .gap(px(8.))
        .child(builtin)
        .children(p.stored.iter().map(|s| {
            let id = s["policy_id"].as_str().unwrap_or("").to_string();
            let doc: Option<Value> = s["bytes"]
                .as_str()
                .and_then(|b| serde_json::from_str(b).ok());
            let title = doc
                .as_ref()
                .and_then(|d| d["name"].as_str())
                .filter(|n| !n.is_empty())
                .map(String::from)
                .unwrap_or_else(|| id.clone());
            let rules = doc
                .as_ref()
                .and_then(|d| d["rules"].as_array())
                .map(Vec::len)
                .unwrap_or(0);
            let enabled = s["enabled"].as_bool().unwrap_or(false);
            let valid = s["valid"].as_bool().unwrap_or(true);
            let line = if valid {
                format!(
                    "{rules} rule{} · {}",
                    if rules == 1 { "" } else { "s" },
                    if enabled { "active" } else { "paused" }
                )
            } else {
                format!("Not running: {}", s["error"].as_str().unwrap_or("invalid"))
            };
            let (t, r) = (id.clone(), id.clone());
            row(false)
                .child(div().flex_1().min_w_0().child(name(title)).child(sub(
                    line,
                    if valid {
                        muted()
                    } else {
                        c(crate::timeline::DANGER)
                    },
                )))
                .child(
                    div()
                        .id(SharedString::from(format!("pol-toggle-{id}")))
                        .relative()
                        .w(px(40.))
                        .h(px(22.))
                        .flex_shrink_0()
                        .rounded(px(11.))
                        .bg(if enabled { c(ACCENT) } else { white_a(0.16) })
                        .cursor_pointer()
                        .on_click(click(on, move || Act::Toggle(t.clone())))
                        .child(
                            div()
                                .absolute()
                                .top(px(3.))
                                .left(px(if enabled { 21. } else { 3. }))
                                .size(px(16.))
                                .rounded_full()
                                .bg(c(0xffffff)),
                        ),
                )
                .child(
                    div()
                        .id(SharedString::from(format!("pol-delete-{id}")))
                        .p(px(6.))
                        .flex_shrink_0()
                        .rounded(px(6.))
                        .text_size(px(14.))
                        .line_height(px(16.))
                        .text_color(muted())
                        .cursor_pointer()
                        .hover(|d| {
                            d.text_color(c(crate::timeline::DANGER))
                                .bg(c(crate::timeline::DANGER).opacity(0.1))
                        })
                        .on_click(click(on, move || Act::Remove(r.clone())))
                        .child("✕"),
                )
        }))
}

fn gallery(p: &Policies, width: f32, on: &OnAct) -> impl IntoElement {
    let cols = (((width + 10.) / 230.).floor() as u16).max(1);
    let shown: Vec<&Value> = p
        .presets
        .iter()
        .filter(|x| !matches!(x["id"].as_str(), Some("blank" | "vector_defaults")))
        .collect();
    let card = |x: &Value| {
        let id = x["id"].as_str().unwrap_or("").to_string();
        div()
            .id(SharedString::from(format!("pol-card-{id}")))
            .flex()
            .flex_col()
            .items_start()
            .p(px(14.))
            .rounded(px(12.))
            .bg(white_a(0.04))
            .border_1()
            .border_color(white_a(0.07))
            .cursor_pointer()
            .hover(|d| d.border_color(c(ACCENT)))
            .on_click(click(on, move || Act::Open(id.clone())))
            .child(
                div()
                    .mb(px(4.))
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_size(px(14.))
                    .line_height(px(24.))
                    .text_color(c(0xf6f6f6))
                    .child(x["name"].as_str().unwrap_or("").to_string()),
            )
            .child(
                div()
                    .text_size(px(12.))
                    .line_height(px(17.4))
                    .text_color(muted())
                    .child(x["description"].as_str().unwrap_or("").to_string()),
            )
            .child(
                div()
                    .mt(px(6.))
                    .text_size(px(11.))
                    .line_height(px(13.2))
                    .italic()
                    .text_color(muted().opacity(0.85))
                    .child(format!("Catches: {}", x["example"].as_str().unwrap_or(""))),
            )
    };
    let blank = p.presets.iter().find(|x| x["id"] == "blank");
    div()
        .flex()
        .flex_col()
        .child(
            crate::kit::spaced("START FROM A TEMPLATE", 0.95)
                .mb(px(8.))
                .text_size(px(10.5))
                .line_height(px(24.))
                .text_color(white_a(0.38)),
        )
        .child(
            div()
                .pb(px(4.))
                .grid()
                .grid_cols(cols)
                .gap(px(10.))
                .children(shown.into_iter().map(card)),
        )
        .children(blank.map(|b| {
            div().mt(px(16.)).mb(px(18.)).child(
                div()
                    .id("pol-scratch")
                    .w_full()
                    .flex()
                    .items_center()
                    .gap(px(12.))
                    .px(px(14.))
                    .py(px(13.))
                    .rounded(px(12.))
                    .border_1()
                    .border_dashed()
                    .border_color(white_a(0.14))
                    .cursor_pointer()
                    .hover(|d| d.border_color(ca(89, 252, 179, 0.45)).bg(white_a(0.02)))
                    .on_click(click(on, || Act::Open("blank".into())))
                    .child(
                        div()
                            .w(px(22.))
                            .text_center()
                            .text_size(px(19.))
                            .line_height(px(19.))
                            .text_color(ca(89, 252, 179, 0.8))
                            .child("+"),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap(px(2.))
                            .min_w_0()
                            .child(
                                div()
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .text_size(px(14.))
                                    .line_height(px(24.))
                                    .text_color(c(0xf6f6f6))
                                    .child(b["name"].as_str().unwrap_or("").to_string()),
                            )
                            .child(
                                div()
                                    .text_size(px(12.))
                                    .line_height(px(24.))
                                    .text_color(muted())
                                    .child(b["description"].as_str().unwrap_or("").to_string()),
                            ),
                    ),
            )
        }))
}

fn field(input: &Entity<TextInput>, multiline: bool) -> impl IntoElement {
    div()
        .w_full()
        .px(px(12.))
        .py(px(10.))
        .when(multiline, |d| d.min_h(px(86.)))
        .when(!multiline, |d| d.h(px(45.)).flex().items_center())
        .rounded(px(8.))
        .border_1()
        .border_color(white_a(0.1))
        .bg(ca(0, 0, 0, 0.25))
        .child(input.clone())
}

fn dial(p: &Policies, mo: &Moderation, d: &Value, on: &OnAct) -> AnyElement {
    let Some(draft) = &p.draft else {
        return div().into_any_element();
    };
    let kind = d["kind"].as_str().unwrap_or("");
    let key = d["key"].as_str().unwrap_or("");
    let label = div()
        .mb(px(-5.))
        .flex()
        .items_center()
        .gap(px(6.))
        .font_weight(FontWeight::SEMIBOLD)
        .text_size(px(13.))
        .line_height(px(24.))
        .text_color(c(0xf6f6f6))
        .child(d["label"].as_str().unwrap_or("").to_string())
        .when(kind == "strictness", |l| {
            l.child(
                div()
                    .id("pol-info")
                    .group("pol-info")
                    .cursor_pointer()
                    .on_click(click(on, || Act::Explain))
                    .child(
                        svg()
                            .path("icons/info.svg")
                            .size(px(15.))
                            .text_color(white_a(0.4))
                            .group_hover("pol-info", |s| s.text_color(c(0xffffff))),
                    ),
            )
        });
    let hint = div()
        .mb(px(6.))
        .text_size(px(11.5))
        .line_height(px(24.))
        .text_color(muted())
        .child(d["hint"].as_str().unwrap_or("").to_string());
    let body: AnyElement = match kind {
        "summary" => {
            let n = draft.summary.len();
            div()
                .flex()
                .flex_col()
                .gap(px(1.))
                .children(draft.summary.iter().enumerate().map(|(i, r)| {
                    let armed = r["armed"].as_bool().unwrap_or(false);
                    div()
                        .px(px(12.))
                        .py(px(9.))
                        .bg(white_a(0.03))
                        .border_l(px(2.))
                        .border_color(if armed {
                            white_a(0.14)
                        } else {
                            ca(89, 252, 179, 0.45)
                        })
                        .when(i == 0, |d| d.rounded_tr(px(8.)))
                        .when(i + 1 == n, |d| d.rounded_br(px(8.)))
                        .child(
                            div()
                                .flex()
                                .flex_wrap()
                                .items_baseline()
                                .gap(px(9.))
                                .font_weight(FontWeight::SEMIBOLD)
                                .text_size(px(13.))
                                .line_height(px(24.))
                                .text_color(c(0xf6f6f6))
                                .child(r["label"].as_str().unwrap_or("").to_string())
                                .when(armed, |d| {
                                    d.child(
                                        div()
                                            .font_weight(FontWeight::MEDIUM)
                                            .text_size(px(10.5))
                                            .text_color(white_a(0.34))
                                            .child("only after another rule fires"),
                                    )
                                }),
                        )
                        .child(
                            div()
                                .text_size(px(12.))
                                .line_height(px(17.4))
                                .text_color(white_a(0.46))
                                .child(r["detail"].as_str().unwrap_or("").to_string()),
                        )
                }))
                .into_any_element()
        }
        "strictness" => {
            let says = STRICTNESS
                .iter()
                .find(|s| s.id == draft.strictness)
                .unwrap_or(&STRICTNESS[1])
                .says;
            div()
                .child(
                    div()
                        .flex()
                        .gap(px(6.))
                        .children(STRICTNESS.iter().map(|s| {
                            let active = draft.strictness == s.id;
                            let id = s.id;
                            div()
                                .id(SharedString::from(format!("pol-seg-{id}")))
                                .flex_1()
                                .px(px(10.))
                                .py(px(8.))
                                .rounded(px(8.))
                                .border_1()
                                .border_color(if active { c(ACCENT) } else { white_a(0.1) })
                                .bg(if active {
                                    ca(89, 252, 179, 0.08)
                                } else {
                                    ca(0, 0, 0, 0.2)
                                })
                                .flex()
                                .justify_center()
                                .font_weight(FontWeight::SEMIBOLD)
                                .text_size(px(12.5))
                                .line_height(px(15.))
                                .text_color(if active { c(ACCENT) } else { muted() })
                                .cursor_pointer()
                                .on_click(click(on, move || Act::Strictness(id)))
                                .child(s.label)
                        })),
                )
                .child(
                    div()
                        .mt(px(8.))
                        .text_size(px(12.))
                        .line_height(px(18.))
                        .text_color(white_a(0.42))
                        .child(says),
                )
                .into_any_element()
        }
        "wordlist" | "domainlist" | "text" | "seconds" => match draft.inputs.get(key) {
            Some(input) if kind == "seconds" => div()
                .flex()
                .items_center()
                .gap(px(8.))
                .child(div().w(px(110.)).child(field(input, false)))
                .child(
                    div()
                        .text_size(px(12.5))
                        .text_color(muted())
                        .child("seconds"),
                )
                .into_any_element(),
            Some(input) => field(input, kind != "text").into_any_element(),
            None => div().into_any_element(),
        },
        "rules" => rules(p, on),
        "channels" => {
            let channels: Vec<(String, String)> = mo
                .intel
                .as_ref()
                .and_then(|i| i["channels"].as_array())
                .into_iter()
                .flatten()
                .map(|c| {
                    (
                        c["id"].as_str().unwrap_or("").to_string(),
                        c["name"]
                            .as_str()
                            .filter(|n| !n.is_empty())
                            .unwrap_or("channel")
                            .to_string(),
                    )
                })
                .collect();
            if channels.is_empty() {
                div()
                    .text_size(px(11.5))
                    .text_color(muted())
                    .child("This community has no channels.")
                    .into_any_element()
            } else {
                div()
                    .flex()
                    .flex_wrap()
                    .gap(px(6.))
                    .children(channels.into_iter().map(|(id, name)| {
                        let on_ = draft.channels.contains(&id);
                        div()
                            .id(SharedString::from(format!("pol-chip-{id}")))
                            .px(px(10.))
                            .py(px(6.))
                            .rounded_full()
                            .border_1()
                            .border_color(if on_ { c(ACCENT) } else { white_a(0.12) })
                            .bg(ca(0, 0, 0, 0.2))
                            .text_size(px(12.))
                            .line_height(px(14.))
                            .text_color(if on_ { c(ACCENT) } else { muted() })
                            .cursor_pointer()
                            .on_click(click(on, move || Act::Channel(id.clone())))
                            .child(format!("#{name}"))
                    }))
                    .into_any_element()
            }
        }
        _ => div().into_any_element(),
    };
    div()
        .flex()
        .flex_col()
        .child(label)
        .child(hint)
        .child(body)
        .into_any_element()
}

fn rules(p: &Policies, on: &OnAct) -> AnyElement {
    let Some(draft) = &p.draft else {
        return div().into_any_element();
    };
    let mut out = div().flex().flex_col().gap(px(10.));
    for (i, r) in draft.rules.iter().enumerate() {
        let Some(kind) = p.kind(&r.kind) else {
            continue;
        };
        let input = kind["input"].as_str().unwrap_or("none");
        out = out.child(
            div()
                .px(px(13.))
                .py(px(12.))
                .rounded(px(10.))
                .bg(white_a(0.035))
                .border_1()
                .border_color(white_a(0.08))
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(10.))
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .font_weight(FontWeight::SEMIBOLD)
                                .text_size(px(13.5))
                                .line_height(px(20.))
                                .text_color(c(0xf6f6f6))
                                .child(kind["label"].as_str().unwrap_or("").to_string()),
                        )
                        .child(
                            div()
                                .id(SharedString::from(format!("pol-rule-x-{i}")))
                                .px(px(4.))
                                .py(px(2.))
                                .rounded(px(6.))
                                .text_size(px(13.))
                                .text_color(white_a(0.35))
                                .cursor_pointer()
                                .hover(|d| d.text_color(c(0xfc595c)))
                                .on_click(click(on, move || Act::RemoveRule(i)))
                                .child("✕"),
                        ),
                )
                .child(
                    div()
                        .mt(px(3.))
                        .text_size(px(12.))
                        .line_height(px(17.4))
                        .text_color(white_a(0.44))
                        .child(kind["description"].as_str().unwrap_or("").to_string()),
                )
                .when(input != "none", |d| {
                    d.child(
                        div()
                            .mt(px(9.))
                            .mb(px(5.))
                            .text_size(px(11.5))
                            .line_height(px(15.))
                            .text_color(white_a(0.36))
                            .child(kind["input_hint"].as_str().unwrap_or("").to_string()),
                    )
                    .child(if input == "seconds" {
                        div()
                            .flex()
                            .items_center()
                            .gap(px(8.))
                            .child(div().w(px(110.)).child(field(&r.input, false)))
                            .child(
                                div()
                                    .text_size(px(12.5))
                                    .text_color(muted())
                                    .child("seconds"),
                            )
                            .into_any_element()
                    } else {
                        field(&r.input, true).into_any_element()
                    })
                }),
        );
    }
    out.child(
        div()
            .flex()
            .flex_wrap()
            .gap(px(7.))
            .children(p.kinds.iter().map(|k| {
                let id = k["id"].as_str().unwrap_or("").to_string();
                let key = id.clone();
                div()
                    .id(SharedString::from(format!("pol-add-{id}")))
                    .px(px(11.))
                    .py(px(7.))
                    .rounded(px(8.))
                    .border_1()
                    .border_dashed()
                    .border_color(white_a(0.16))
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_size(px(12.))
                    .line_height(px(14.))
                    .text_color(white_a(0.6))
                    .cursor_pointer()
                    .hover(|d| {
                        d.border_color(ca(89, 252, 179, 0.45))
                            .text_color(c(0xffffff))
                    })
                    .on_click(click(on, move || Act::AddRule(key.clone())))
                    .child(format!("+ {}", k["label"].as_str().unwrap_or("")))
            })),
    )
    .into_any_element()
}

fn preview(p: &Policies, mo: &Moderation, store: &crate::data::Store) -> AnyElement {
    let frame = || {
        div()
            .p(px(14.))
            .rounded(px(10.))
            .bg(white_a(0.04))
            .border_1()
            .border_color(white_a(0.07))
            .text_size(px(13.))
            .line_height(px(20.))
            .text_color(c(0xf6f6f6))
    };
    let warn = || {
        div()
            .mt(px(10.))
            .px(px(12.))
            .py(px(10.))
            .border_l(px(3.))
            .border_color(c(crate::timeline::DANGER))
            .bg(c(crate::timeline::DANGER).opacity(0.1))
            .rounded_r(px(8.))
            .text_size(px(12.5))
            .line_height(px(19.))
    };
    if let Some(e) = &p.preview_error {
        return frame()
            .child(warn().mt_0().child(format!("Not ready: {e}")))
            .into_any_element();
    }
    let Some(res) = &p.preview else {
        return div().into_any_element();
    };
    let name = |npub: &str| {
        let p = store.profiles.get(npub);
        match p {
            Some(p) if !p.nickname.is_empty() => p.nickname.clone(),
            Some(p) if !p.name.is_empty() => p.name.clone(),
            Some(p) if !p.display_name.is_empty() => p.display_name.clone(),
            _ if npub.len() > 14 => format!("{}…{}", &npub[..10], &npub[npub.len() - 4..]),
            _ => npub.to_string(),
        }
    };
    let _ = mo;
    let plural = |n: u64, w: &str| format!("{w}{}", if n == 1 { "" } else { "s" });
    let flagged = res["flagged"].as_array().cloned().unwrap_or_default();
    let n = flagged.len() as u64;
    let cited = res["messages_cited"].as_u64().unwrap_or(0);
    let head = format!(
        "Over the last 7 days this would have flagged {n} {} and cited {cited} {}.",
        plural(n, "member"),
        plural(cited, "message")
    );
    let n_text = n.to_string();
    let c_text = cited.to_string();
    let a = head
        .find(&format!(" {n_text} "))
        .map(|i| i + 1)
        .unwrap_or(0);
    let b = head
        .rfind(&format!(" {c_text} "))
        .map(|i| i + 1)
        .unwrap_or(0);
    let accent = gpui::HighlightStyle {
        color: Some(c(ACCENT)),
        font_weight: Some(FontWeight::BOLD),
        ..Default::default()
    };
    let mut out = frame().child(
        div().mb(px(8.)).font_weight(FontWeight::SEMIBOLD).child(
            gpui::StyledText::new(head.clone())
                .with_highlights([(a..a + n_text.len(), accent), (b..b + c_text.len(), accent)]),
        ),
    );
    if n == 0 {
        let corpus = res["corpus"].as_u64().unwrap_or(0);
        out = out.child(div().text_size(px(11.5)).line_height(px(17.)).text_color(muted()).child(format!(
            "Checked against {corpus} {}, and nothing tripped this rule. Usually good news, though it also means the preview cannot tell you whether the rule works. Try a word you know someone has used.",
            plural(corpus, "message")
        )));
    }
    let shielded = res["shielded_matches"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    if !shielded.is_empty() {
        let k = shielded.len() as u64;
        let names: Vec<String> = shielded
            .iter()
            .take(4)
            .map(|r| {
                format!(
                    "{} ({} msgs)",
                    name(r["npub"].as_str().unwrap_or("")),
                    r["messages"].as_u64().unwrap_or(0)
                )
            })
            .collect();
        out = out.child(warn().child(format!(
            "{k} trusted {} also matched this rule and {} spared only by their standing: {}{}. If this rule is meant for raiders, it is catching ordinary conversation.",
            plural(k, "member"),
            if k == 1 { "was" } else { "were" },
            names.join(", "),
            if k > 4 { ", …" } else { "" }
        )));
    }
    let unevaluated: Vec<&str> = res["unevaluated"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .collect();
    if !unevaluated.is_empty() {
        out = out.child(
            div()
                .mt(px(8.))
                .text_size(px(11.5))
                .text_color(muted())
                .child(format!("Not checked here: {}", unevaluated.join(", "))),
        );
    }
    if n > 0 {
        out = out.child(div().mt(px(10.)).flex().flex_col().gap(px(6.)).children(
            flagged.iter().take(30).map(|r| {
                let proven = r["proven"].as_u64().unwrap_or(1);
                div()
                    .flex()
                    .items_baseline()
                    .gap(px(8.))
                    .px(px(8.))
                    .py(px(6.))
                    .rounded(px(6.))
                    .bg(ca(0, 0, 0, 0.18))
                    .child(
                        div()
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_size(px(12.5))
                            .child(name(r["npub"].as_str().unwrap_or(""))),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_size(px(11.5))
                            .text_color(muted())
                            .child(r["reasons"][0].as_str().unwrap_or("").to_string()),
                    )
                    .child(div().text_size(px(11.)).text_color(muted()).child(format!(
                        "{}{}",
                        r["score"].as_u64().unwrap_or(0),
                        if proven == 0 {
                            " · suspected"
                        } else {
                            " · provable"
                        }
                    )))
            }),
        ));
    }
    out.into_any_element()
}

fn editor(
    p: &Policies,
    mo: &Moderation,
    store: &crate::data::Store,
    on: &OnAct,
    cx: &App,
) -> AnyElement {
    let Some(draft) = &p.draft else {
        return div().into_any_element();
    };
    let ready = readiness(p, cx);
    let can_preview = !p.busy && ready.is_ok();
    let preview_btn = div()
        .id("pol-preview")
        .min_w(px(232.))
        .justify_center()
        .px(px(14.))
        .py(px(9.))
        .flex()
        .items_center()
        .gap(px(7.))
        .rounded(px(10.))
        .border_1()
        .border_color(white_a(0.12))
        .bg(white_a(0.05))
        .font_weight(FontWeight::SEMIBOLD)
        .text_size(px(13.))
        .line_height(px(16.))
        .text_color(c(0xf6f6f6))
        .when(!can_preview, |d| d.opacity(0.38))
        .when(can_preview, |d| {
            d.cursor_pointer()
                .hover(|d| d.bg(white_a(0.1)))
                .on_click(click(on, || Act::Preview))
        })
        .child(if p.busy {
            "Checking real history…"
        } else {
            "Preview on real history"
        });
    let enable = div()
        .id("pol-enable")
        .min_w(px(196.))
        .px(px(14.))
        .py(px(9.))
        .flex()
        .justify_center()
        .rounded(px(10.))
        .border_1()
        .font_weight(FontWeight::SEMIBOLD)
        .text_size(px(13.))
        .line_height(px(16.))
        .when(p.previewed, |d| {
            d.bg(c(ACCENT))
                .border_color(gpui::transparent_black())
                .text_color(c(0x05261a))
                .cursor_pointer()
                .on_click(click(on, || Act::Save))
        })
        .when(!p.previewed, |d| {
            d.border_color(white_a(0.12)).text_color(white_a(0.32))
        })
        .child(if p.previewed {
            "Enable this policy"
        } else {
            "Preview before Enabling"
        });
    div()
        .flex()
        .flex_col()
        .gap(px(14.))
        .child(
            div()
                .ml(px(-4.))
                .flex()
                .items_center()
                .gap(px(8.))
                .child(
                    div()
                        .id("pol-back")
                        .size(px(30.))
                        .flex_shrink_0()
                        .rounded_full()
                        .flex()
                        .items_center()
                        .justify_center()
                        .cursor_pointer()
                        .group("pol-back")
                        .hover(|d| d.bg(white_a(0.07)))
                        .on_click(click(on, || Act::Back))
                        .child(
                            svg()
                                .path("icons/chevron-double-left.svg")
                                .size(px(16.))
                                .text_color(white_a(0.5))
                                .group_hover("pol-back", |s| s.text_color(c(0xffffff))),
                        ),
                )
                .child(
                    div()
                        .font_weight(FontWeight::BOLD)
                        .text_size(px(16.))
                        .line_height(px(24.))
                        .text_color(c(0xf6f6f6))
                        .child(draft.name.clone()),
                ),
        )
        .child(
            div()
                .px(px(12.))
                .py(px(10.))
                .rounded_r(px(8.))
                .border_l(px(3.))
                .border_color(ca(255, 193, 7, 0.6))
                .bg(ca(255, 193, 7, 0.08))
                .text_size(px(12.5))
                .line_height(px(18.75))
                .text_color(c(0xf6f6f6))
                .child(draft.caveat.clone()),
        )
        .child(
            div()
                .flex()
                .flex_col()
                .gap(px(14.))
                .children(draft.dials.iter().map(|d| dial(p, mo, d, on))),
        )
        .when(p.preview_error.is_some() || p.previewed, |d| {
            d.child(preview(p, mo, store))
        })
        .child(
            div()
                .py(px(13.))
                .border_t_1()
                .border_color(ca(57, 57, 57, 0.5))
                .flex()
                .justify_end()
                .items_center()
                .gap(px(10.))
                .child(preview_btn)
                .child(enable),
        )
        .into_any_element()
}

pub fn view(
    p: &Policies,
    mo: &Moderation,
    store: &crate::data::Store,
    width: f32,
    on: &OnAct,
    cx: &App,
) -> AnyElement {
    let body = if p.draft.is_some() {
        editor(p, mo, store, on, cx)
    } else {
        div()
            .flex()
            .flex_col()
            .gap(px(14.))
            .child(list(p, on))
            .child(gallery(p, width, on))
            .into_any_element()
    };
    div()
        .id("mod-policies")
        .flex_1()
        .min_h_0()
        .overflow_y_scroll()
        .px(px(18.))
        .pt(px(14.))
        .child(body)
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_and_splits() {
        assert_eq!(new_id("My Rule!", &[]), "my-rule");
        assert_eq!(new_id("x", &[json!({"policy_id": "x"})]), "x-2");
        assert_eq!(split("a, b\n\nc"), vec!["a", "b", "c"]);
        assert_eq!(seconds("9999", 10), 3600);
        assert_eq!(seconds("", 7), 7);
    }

    /// Writes the shipped catalogue where `DUMP_PRESETS` says, for the reference mock.
    #[test]
    #[ignore]
    fn dump_presets() {
        let Ok(path) = std::env::var("DUMP_PRESETS") else {
            return;
        };
        std::fs::write(
            path,
            vector_core::VectorCore
                .policy_presets()
                .unwrap()
                .to_string(),
        )
        .unwrap();
    }
}
