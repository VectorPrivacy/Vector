//! The Community Moderation console (`moderation/ModConsole.svelte`): raid triage and the two
//! containment verbs, a banlist edition and a key rotation. Ticked is kept; the unticked set is
//! what a rotation cuts.

use std::collections::HashSet;
use std::rc::Rc;

use gpui::prelude::*;
use gpui::{AnyElement, App, Entity, FontWeight, ScrollHandle, SharedString, Window, div, px, svg};
use serde_json::Value;

use crate::composer::TextInput;
use crate::data::Store;
use crate::theme::*;

const RED: u32 = 0xfc595c;
const GOLD: u32 = 0xfce459;
const AMBER: u32 = 0xfca559;

#[derive(Clone, Copy, PartialEq)]
pub enum Tab {
    Members,
    Policies,
}

pub struct Moderation {
    pub community: String,
    pub intel: Option<Value>,
    pub loading: bool,
    pub error: Option<String>,
    pub keep: HashSet<String>,
    pub filter: &'static str,
    pub query: Entity<TextInput>,
    pub busy: Option<(String, String)>,
    pub tab: Tab,
    pub scroll: ScrollHandle,
    pub policies: crate::policies::Policies,
}

pub enum Act {
    Close,
    Tab(Tab),
    Filter(&'static str),
    Keep(String),
    Revoke,
    Rotate,
    BanRotate,
    Policy(crate::policies::Act),
}

pub type OnAct = Rc<dyn Fn(Act, &mut Window, &mut App)>;

fn click(
    on: &OnAct,
    act: impl Fn() -> Act + 'static,
) -> impl Fn(&gpui::ClickEvent, &mut Window, &mut App) + 'static {
    let on = on.clone();
    move |_, w, cx| on(act(), w, cx)
}

impl Moderation {
    pub fn members(&self) -> &[Value] {
        self.intel
            .as_ref()
            .and_then(|i| i["report"]["members"].as_array())
            .map(Vec::as_slice)
            .unwrap_or(&[])
    }

    pub fn cut(&self) -> Vec<String> {
        self.members()
            .iter()
            .filter_map(|m| m["npub"].as_str())
            .filter(|n| !self.keep.contains(*n))
            .map(String::from)
            .collect()
    }

    pub fn invites(&self) -> usize {
        self.intel
            .as_ref()
            .and_then(|i| i["invites"].as_array())
            .map(Vec::len)
            .unwrap_or(0)
    }

    /// Protected and trusted start ticked, convicted start clear; `prev` keeps every tick the
    /// operator already gave.
    pub fn set_intel(&mut self, intel: Value, refresh: bool) {
        let seen: HashSet<String> = if refresh {
            self.members()
                .iter()
                .filter_map(|m| m["npub"].as_str().map(String::from))
                .collect()
        } else {
            HashSet::new()
        };
        let keep = intel["report"]["members"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|m| {
                let n = m["npub"].as_str().unwrap_or("");
                if seen.contains(n) {
                    self.keep.contains(n)
                } else {
                    m["verdict"].as_str() != Some("suspect")
                }
            })
            .filter_map(|m| m["npub"].as_str().map(String::from))
            .collect();
        self.keep = keep;
        self.intel = Some(intel);
        self.loading = false;
        self.error = None;
    }
}

/// `modAgo`: relative time that stays readable at raid speed.
pub fn ago(secs: u64) -> String {
    match secs {
        0 => "unknown".into(),
        s if s < 60 => format!("{s}s"),
        s if s < 3600 => format!("{}m", s / 60),
        s if s < 86400 => format!("{}h", s / 3600),
        s if s < 86400 * 365 => format!("{}d", s / 86400),
        s => format!("{:.1}y", s as f64 / (86400. * 365.)),
    }
}

/// `modDisplayName`: your nickname for them, theirs here, their name, or a short key.
fn display_name(store: &Store, cid: &str, npub: &str) -> String {
    let p = store.profiles.get(npub);
    let nick = store
        .nicknames
        .get(cid)
        .and_then(|n| n.get(npub))
        .filter(|n| !n.is_empty());
    match p {
        Some(p) if !p.nickname.is_empty() => p.nickname.clone(),
        _ if nick.is_some() => nick.cloned().unwrap_or_default(),
        Some(p) if !p.name.is_empty() => p.name.clone(),
        Some(p) if !p.display_name.is_empty() => p.display_name.clone(),
        _ if npub.len() > 14 => format!("{}…{}", &npub[..10], &npub[npub.len() - 4..]),
        _ => npub.to_string(),
    }
}

fn head(m: &Moderation, on: &OnAct) -> impl IntoElement {
    let name = m
        .intel
        .as_ref()
        .map(|i| {
            i["name"]
                .as_str()
                .filter(|n| !n.is_empty())
                .unwrap_or("Community")
                .to_string()
        })
        .unwrap_or_default();
    let epoch = m
        .intel
        .as_ref()
        .map(|i| format!("Epoch {}", i["epoch"].as_u64().unwrap_or(0)));
    div()
        .flex_shrink_0()
        .flex()
        .items_center()
        .gap(px(12.))
        .px(px(18.))
        .py(px(16.))
        .border_b_1()
        .border_color(ca(57, 57, 57, 0.5))
        .child(
            div()
                .flex_1()
                .min_w_0()
                .flex()
                .flex_col()
                .gap(px(2.))
                .child(
                    crate::kit::spaced("MODERATION", 1.6)
                        .font_weight(FontWeight::BOLD)
                        .text_size(px(10.))
                        .line_height(px(24.))
                        .text_color(c(crate::timeline::DANGER)),
                )
                .child(
                    div()
                        .truncate()
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_size(px(17.))
                        .line_height(px(22.))
                        .text_color(c(0xf6f6f6))
                        .child(name),
                ),
        )
        .children(epoch.map(|e| {
            div()
                .px(px(10.))
                .py(px(3.))
                .rounded_full()
                .border_1()
                .border_color(white_a(0.12))
                .text_size(px(11.))
                .line_height(px(16.))
                .text_color(white_a(0.45))
                .child(e)
        }))
        .child(
            div()
                .id("mod-x")
                .px(px(6.))
                .py(px(4.))
                .text_size(px(15.))
                .line_height(px(18.))
                .text_color(white_a(0.5))
                .when(m.busy.is_none(), |d| {
                    d.cursor_pointer()
                        .hover(|d| d.text_color(c(0xffffff)))
                        .on_click(click(on, || Act::Close))
                })
                .child("✕"),
        )
}

fn alert(m: &Moderation) -> Option<impl IntoElement> {
    let (title, body, working) = if let Some((t, b)) = &m.busy {
        (t.clone(), b.clone(), true)
    } else {
        let r = &m.intel.as_ref()?["report"];
        if !r["raid_detected"].as_bool().unwrap_or(false) {
            return None;
        }
        let biggest = r["cohorts"].get(0);
        let (size, from, to) = (
            r["burst_size"].as_u64().unwrap_or(0),
            r["burst_from_ms"].as_u64().unwrap_or(0),
            r["burst_to_ms"].as_u64().unwrap_or(0),
        );
        let burst = if size >= 2 && to > from {
            format!(
                " {size} joined within {} of each other.",
                ago(((to - from) as f64 / 1000.).round() as u64)
            )
        } else {
            String::new()
        };
        let cohort = biggest
            .map(|b| {
                let sample: String = b["sample"]
                    .as_str()
                    .unwrap_or("")
                    .chars()
                    .take(40)
                    .collect();
                format!(" {} posted “{sample}”.", b["size"].as_u64().unwrap_or(0))
            })
            .unwrap_or_default();
        (
            format!(
                "Raid: {} accounts flagged.",
                r["suspects"].as_u64().unwrap_or(0)
            ),
            format!("{cohort}{burst} They start unticked, so they are the ones being removed."),
            false,
        )
    };
    let tone = c(crate::timeline::DANGER);
    let mut text = title.clone();
    text.push(' ');
    let bold = 0..title.len();
    text.push_str(body.trim_start());
    Some(
        div()
            .mx(px(18.))
            .mt(px(12.))
            .px(px(13.))
            .py(px(11.))
            .flex()
            .items_start()
            .gap(px(10.))
            .rounded(px(10.))
            .border_1()
            .when(working, |d| d.bg(white_a(0.05)).border_color(white_a(0.14)))
            .when(!working, |d| {
                d.bg(tone.opacity(0.12)).border_color(tone.opacity(0.38))
            })
            .child(
                svg()
                    .path("icons/vector_warning.svg")
                    .mt(px(2.))
                    .size(px(16.))
                    .flex_shrink_0()
                    .text_color(if working { white_a(0.6) } else { tone }),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .text_size(px(13.))
                    .line_height(px(19.))
                    .text_color(white_a(0.72))
                    .child(gpui::StyledText::new(text).with_highlights([(
                        bold,
                        gpui::HighlightStyle {
                            color: Some(c(0xf6f6f6)),
                            font_weight: Some(FontWeight::BOLD),
                            ..Default::default()
                        },
                    )])),
            ),
    )
}

fn tabs(m: &Moderation, on: &OnAct) -> impl IntoElement {
    let tab = |id: &'static str, label: &'static str, which: Tab| {
        let active = m.tab == which;
        div()
            .id(id)
            .px(px(2.))
            .py(px(11.))
            .mb(px(-1.))
            .border_b_2()
            .border_color(if active {
                c(ACCENT)
            } else {
                gpui::transparent_black()
            })
            .font_weight(FontWeight::SEMIBOLD)
            .text_size(px(14.))
            .line_height(px(17.))
            .text_color(if active { c(0xffffff) } else { white_a(0.42) })
            .cursor_pointer()
            .when(!active, |d| d.hover(|d| d.text_color(white_a(0.72))))
            .on_click(click(on, move || Act::Tab(which)))
            .child(label)
    };
    div()
        .flex_shrink_0()
        .px(px(18.))
        .flex()
        .gap(px(22.))
        .border_b_1()
        .border_color(white_a(0.08))
        .child(tab("mod-tab-members", "Members", Tab::Members))
        .child(tab("mod-tab-policies", "Policies", Tab::Policies))
}

fn stats(m: &Moderation) -> impl IntoElement {
    let cells: Vec<(u64, &str, gpui::Hsla)> = match &m.intel {
        None => Vec::new(),
        Some(i) => {
            let r = &i["report"];
            let unlisted: HashSet<&str> = i["unlisted"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
                .collect();
            let members = m.members();
            let flagged = members
                .iter()
                .filter(|x| x["verdict"].as_str() == Some("suspect"))
                .count() as u64;
            vec![
                (
                    members
                        .iter()
                        .filter(|x| !unlisted.contains(x["npub"].as_str().unwrap_or("")))
                        .count() as u64,
                    "MEMBERS",
                    white_a(0.92),
                ),
                (r["trusted"].as_u64().unwrap_or(0), "TRUSTED", c(ACCENT)),
                (r["protected"].as_u64().unwrap_or(0), "STAFF", c(GOLD)),
                (
                    flagged,
                    "FLAGGED",
                    if flagged > 0 { c(RED) } else { white_a(0.92) },
                ),
            ]
        }
    };
    div()
        .flex_shrink_0()
        .mx(px(18.))
        .mt(px(14.))
        .flex()
        .rounded(px(12.))
        .overflow_hidden()
        .bg(white_a(0.028))
        .border_1()
        .border_color(white_a(0.06))
        .children(cells.into_iter().enumerate().map(|(i, (n, label, tone))| {
            div()
                .flex_1()
                .relative()
                .flex()
                .flex_col()
                .justify_center()
                .gap(px(4.))
                .px(px(14.))
                .py(px(12.))
                .when(i > 0, |d| {
                    d.child(
                        div()
                            .absolute()
                            .left_0()
                            .top(gpui::relative(0.22))
                            .bottom(gpui::relative(0.22))
                            .w(px(1.))
                            .bg(white_a(0.07)),
                    )
                })
                .child(
                    div()
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_size(px(21.))
                        .line_height(px(22.))
                        .text_color(tone)
                        .child(n.to_string()),
                )
                .child(
                    crate::kit::spaced(label, 0.95)
                        .text_size(px(10.5))
                        .line_height(px(11.5))
                        .text_color(white_a(0.4)),
                )
        }))
}

fn chips(m: &Moderation, on: &OnAct) -> impl IntoElement {
    let all = m.members().len();
    let cut = m.cut().len();
    let counts = [
        ("all", "Everyone", all),
        ("cut", "Removing", cut),
        ("keep", "Staying", all - cut),
    ];
    div()
        .flex()
        .gap(px(6.))
        .children(counts.into_iter().map(|(id, label, n)| {
            let active = m.filter == id;
            div()
                .id(SharedString::from(format!("mod-chip-{id}")))
                .px(px(11.))
                .py(px(7.))
                .flex()
                .items_center()
                .gap(px(6.))
                .rounded(px(9.))
                .border_1()
                .border_color(if active {
                    white_a(0.22)
                } else {
                    ca(57, 57, 57, 0.6)
                })
                .bg(if active { white_a(0.09) } else { white_a(0.03) })
                .font_weight(FontWeight::MEDIUM)
                .text_size(px(12.))
                .line_height(px(18.))
                .text_color(if active { c(0xffffff) } else { white_a(0.6) })
                .cursor_pointer()
                .hover(|d| d.bg(white_a(0.07)).text_color(c(0xffffff)))
                .on_click(click(on, move || Act::Filter(id)))
                .child(label)
                .child(
                    div()
                        .text_size(px(11.))
                        .text_color(if active { white_a(0.75) } else { white_a(0.4) })
                        .child(n.to_string()),
                )
        }))
}

fn row(m: &Moderation, store: &Store, x: &Value, on: &OnAct) -> impl IntoElement {
    let npub = x["npub"].as_str().unwrap_or("").to_string();
    let kept = m.keep.contains(&npub);
    let verdict = x["verdict"].as_str().unwrap_or("");
    let locked = verdict == "protected";
    let reasons: Vec<&str> = x["reasons"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .collect();
    let why: Vec<&str> = reasons
        .iter()
        .copied()
        .filter(|r| {
            !matches!(
                *r,
                "Long-standing member" | "Holds a role" | "Community owner"
            )
        })
        .collect();
    let flag = |k: &str| x[k].as_bool().unwrap_or(false);
    let badge = if flag("is_owner") {
        Some("OWNER")
    } else if flag("is_me") {
        Some("YOU")
    } else if flag("is_admin") {
        Some("STAFF")
    } else if verdict == "trusted" {
        Some("REGULAR")
    } else if verdict == "neutral" && !reasons.is_empty() {
        Some("CHECK")
    } else {
        None
    };
    let badge_tone = match verdict {
        "trusted" => Some(c(ACCENT)),
        "protected" => Some(c(GOLD)),
        "neutral" => Some(c(AMBER)),
        _ => None,
    };
    let rail = if !kept {
        Some(c(RED))
    } else {
        match verdict {
            "protected" => Some(c(GOLD)),
            "trusted" => Some(c(ACCENT).opacity(0.55)),
            "suspect" => Some(c(RED)),
            _ => None,
        }
    };
    let num = |n: String| (n, true);
    let mut meta: Vec<(String, bool)> = Vec::new();
    let tenure = x["tenure_secs"].as_u64().unwrap_or(0);
    if tenure > 0 {
        meta.push(("here ".into(), false));
        meta.push(num(ago(tenure)));
    } else {
        meta.push(("age unknown".into(), false));
    }
    let dot = || (" · ".to_string(), false);
    let msgs = x["messages"].as_u64().unwrap_or(0);
    meta.push(dot());
    meta.push(num(msgs.to_string()));
    meta.push((format!(" msg{}", if msgs == 1 { "" } else { "s" }), false));
    let distinct = x["distinct"].as_u64().unwrap_or(0);
    if distinct > 0 {
        meta.push(dot());
        meta.push(num(distinct.to_string()));
        meta.push((" distinct".into(), false));
    }
    if let Some(label) = x["invite_label"].as_str() {
        meta.push(dot());
        meta.push((format!("via {label}"), false));
    }
    let mut text = String::new();
    let mut highlights = Vec::new();
    for (t, strong) in meta {
        let style = if strong {
            Some(gpui::HighlightStyle {
                color: Some(white_a(0.62)),
                ..Default::default()
            })
        } else if t == " · " {
            Some(gpui::HighlightStyle {
                color: Some(white_a(0.22)),
                ..Default::default()
            })
        } else if t == "age unknown" {
            Some(gpui::HighlightStyle {
                color: Some(white_a(0.34)),
                font_style: Some(gpui::FontStyle::Italic),
                ..Default::default()
            })
        } else {
            None
        };
        if let Some(st) = style {
            highlights.push((text.len()..text.len() + t.len(), st));
        }
        text.push_str(&t);
    }
    let text = text.replace(" · ", "  ·  ");
    let highlights = remap(&highlights);
    let busy = m.busy.is_some();
    let key = npub.clone();
    let group = SharedString::from(format!("mod-row-{npub}"));
    div()
        .id(group.clone())
        .group(group.clone())
        .relative()
        .flex()
        .items_center()
        .gap(px(11.))
        .pl(px(14.))
        .pr(px(10.))
        .py(px(6.))
        .rounded(px(10.))
        .when(!kept, |d| d.bg(c(RED).opacity(0.07)))
        .hover(|d| d.bg(white_a(0.045)))
        .when(!locked && !busy, |d| {
            d.cursor_pointer()
                .on_click(click(on, move || Act::Keep(key.clone())))
        })
        .children(rail.map(|r| {
            div()
                .absolute()
                .left(px(3.))
                .top(px(8.))
                .bottom(px(8.))
                .w(px(2.))
                .rounded(px(2.))
                .bg(r)
        }))
        .child(
            div()
                .size(px(19.))
                .flex_shrink_0()
                .rounded(px(6.))
                .flex()
                .items_center()
                .justify_center()
                .when(locked, |d| d.opacity(0.4))
                .when(kept, |d| {
                    d.bg(c(ACCENT).opacity(0.42))
                        .group_hover(group.clone(), |d| d.bg(c(ACCENT)))
                        .child(
                            svg()
                                .path("icons/check.svg")
                                .size(px(13.))
                                .text_color(ca(6, 18, 13, 0.82)),
                        )
                })
                .when(!kept, |d| {
                    d.border(px(1.5)).border_color(c(RED).opacity(0.55))
                }),
        )
        .child(
            div()
                .size(px(30.))
                .flex_shrink_0()
                .when(!kept, |d| d.opacity(0.55))
                .child(crate::ui::avatar(store, &npub, 30.)),
        )
        .child(
            div()
                .flex_1()
                .min_w_0()
                .flex()
                .flex_col()
                .gap(px(1.))
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(7.))
                        .min_w_0()
                        .child(
                            div()
                                .truncate()
                                .font_weight(FontWeight::SEMIBOLD)
                                .text_size(px(13.5))
                                .line_height(px(18.))
                                .text_color(c(0xf6f6f6))
                                .when(!kept, |d| d.opacity(0.72))
                                .child(display_name(store, &m.community, &npub)),
                        )
                        .children(badge.map(|b| {
                            let tone = badge_tone.unwrap_or(white_a(0.6));
                            crate::kit::spaced(b, 0.72)
                                .flex_shrink_0()
                                .px(px(6.))
                                .py(px(2.))
                                .rounded(px(5.))
                                .bg(tone.opacity(0.14))
                                .font_weight(FontWeight::BOLD)
                                .text_size(px(9.))
                                .line_height(px(13.))
                                .text_color(tone)
                        })),
                )
                .child(
                    div()
                        .truncate()
                        .text_size(px(11.))
                        .line_height(px(15.))
                        .text_color(white_a(0.42))
                        .child(gpui::StyledText::new(text).with_highlights(highlights)),
                )
                .when(!why.is_empty(), |d| {
                    d.child(
                        div()
                            .truncate()
                            .text_size(px(11.5))
                            .line_height(px(16.))
                            .text_color(white_a(0.5))
                            .child(why.join(" · ")),
                    )
                }),
        )
}

/// Shifts highlight ranges for the widened " · " separators.
fn remap(
    h: &[(std::ops::Range<usize>, gpui::HighlightStyle)],
) -> Vec<(std::ops::Range<usize>, gpui::HighlightStyle)> {
    let mut shift = 0;
    h.iter()
        .map(|(r, st)| {
            let dot = r.len() == " · ".len() && st.color == Some(white_a(0.22));
            let start = r.start + shift;
            if dot {
                shift += 2;
            }
            (start..r.end + shift, *st)
        })
        .collect()
}

fn list(m: &Moderation, store: &Store, on: &OnAct, cx: &App) -> AnyElement {
    let empty = |text: String| {
        div()
            .h(px(140.))
            .flex()
            .items_center()
            .justify_center()
            .text_size(px(13.))
            .text_color(white_a(0.4))
            .child(text)
            .into_any_element()
    };
    if m.loading {
        return div()
            .h(px(140.))
            .flex()
            .items_center()
            .justify_center()
            .child(
                svg()
                    .path("icons/loading.svg")
                    .size(px(22.))
                    .text_color(white_a(0.4)),
            )
            .into_any_element();
    }
    if let Some(e) = &m.error {
        return empty(e.clone());
    }
    let q = m.query.read(cx).text().trim().to_lowercase();
    let rows: Vec<&Value> = m
        .members()
        .iter()
        .filter(|x| {
            let n = x["npub"].as_str().unwrap_or("");
            match m.filter {
                "cut" if m.keep.contains(n) => return false,
                "keep" if !m.keep.contains(n) => return false,
                _ => {}
            }
            q.is_empty()
                || format!(
                    "{} {} {n}",
                    display_name(store, &m.community, n),
                    store.name(n)
                )
                .to_lowercase()
                .contains(&q)
        })
        .collect();
    if rows.is_empty() {
        return empty("No members match.".into());
    }
    div()
        .flex()
        .flex_col()
        .children(rows.into_iter().map(|x| row(m, store, x, on)))
        .into_any_element()
}

fn mod_btn(
    id: &'static str,
    icon: &'static str,
    label: String,
    danger: bool,
    live: bool,
    on: impl Fn(&gpui::ClickEvent, &mut Window, &mut App) + 'static,
    narrow: bool,
) -> impl IntoElement {
    let red = c(crate::timeline::DANGER);
    div()
        .id(id)
        .when(narrow, |d| d.flex_1().min_w_0().justify_center())
        .px(px(14.))
        .py(px(9.))
        .flex()
        .items_center()
        .gap(px(7.))
        .rounded(px(10.))
        .border_1()
        .when(danger, |d| {
            d.bg(red.opacity(0.16)).border_color(red.opacity(0.45))
        })
        .when(!danger, |d| d.bg(white_a(0.05)).border_color(white_a(0.12)))
        .font_weight(FontWeight::SEMIBOLD)
        .text_size(px(13.))
        .line_height(px(16.))
        .text_color(c(0xf6f6f6))
        .when(!live, |d| d.opacity(0.38))
        .when(live, |d| {
            d.cursor_pointer()
                .when(danger, |d| {
                    d.hover(|d| d.bg(red.opacity(0.26)).border_color(red.opacity(0.65)))
                })
                .when(!danger, |d| {
                    d.hover(|d| d.bg(white_a(0.1)).border_color(white_a(0.24)))
                })
                .on_click(on)
        })
        .child(
            svg()
                .path(icon)
                .size(px(15.))
                .flex_shrink_0()
                .text_color(if danger { red } else { white_a(0.75) }),
        )
        .child(label)
}

fn foot(m: &Moderation, narrow: bool, on: &OnAct) -> impl IntoElement {
    let total = m.members().len();
    let cut = m.cut().len();
    let invites = m.invites();
    let (count, max) = m
        .intel
        .as_ref()
        .map(|i| {
            (
                i["banlist_count"].as_u64().unwrap_or(0) as usize,
                i["banlist_max"].as_u64().unwrap_or(500) as usize,
            )
        })
        .unwrap_or((0, 500));
    let over = cut > max.saturating_sub(count);
    let idle = m.busy.is_none() && m.intel.is_some();
    let n = |b: usize, tone: gpui::Hsla, rest: &'static str| {
        div()
            .flex()
            .items_baseline()
            .gap(px(4.))
            .child(
                div()
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_size(px(13.5))
                    .text_color(tone)
                    .child(b.to_string()),
            )
            .child(rest)
    };
    div()
        .flex_shrink_0()
        .relative()
        .flex()
        .items_center()
        .gap(px(12.))
        .px(px(18.))
        .py(px(13.))
        .mt(px(4.))
        .bg(c(0x101010))
        .border_t_1()
        .border_color(ca(57, 57, 57, 0.5))
        .when(narrow, |d| d.flex_wrap())
        .child(
            div()
                .when(narrow, |d| d.w_full().justify_center())
                .when(!narrow, |d| d.flex_1())
                .min_w_0()
                .flex()
                .items_center()
                .gap(px(14.))
                .text_size(px(12.))
                .text_color(white_a(0.42))
                .child(n(total - cut, c(ACCENT), "staying"))
                .when(cut > 0, |d| {
                    d.child(n(cut, c(crate::timeline::DANGER), "removing"))
                })
                .child(
                    div()
                        .ml_auto()
                        .pr(px(6.))
                        .text_color(white_a(0.3))
                        .children(m.intel.as_ref().map(|_| format!("banlist {count}/{max}"))),
                ),
        )
        .child(
            div()
                .flex()
                .gap(px(8.))
                .when(narrow, |d| d.w_full())
                .child(mod_btn(
                    "mod-revoke",
                    "icons/locked.svg",
                    if invites > 0 {
                        format!(
                            "Revoke {invites} invite{}",
                            if invites == 1 { "" } else { "s" }
                        )
                    } else {
                        "No invite links".into()
                    },
                    false,
                    idle && invites > 0,
                    click(on, || Act::Revoke),
                    narrow,
                ))
                .child(mod_btn(
                    "mod-rotate",
                    "icons/refresh.svg",
                    if cut > 0 {
                        format!("Remove {cut} & rotate")
                    } else {
                        "Rotate keys".into()
                    },
                    false,
                    idle,
                    click(on, || Act::Rotate),
                    narrow,
                ))
                .child(mod_btn(
                    "mod-ban",
                    "icons/x-user.svg",
                    if cut > 0 {
                        format!("Ban {cut} & rotate")
                    } else {
                        "Ban & rotate".into()
                    },
                    true,
                    idle && cut > 0 && !over,
                    click(on, || Act::BanRotate),
                    narrow,
                )),
        )
}

pub fn view(
    m: &Moderation,
    store: &Store,
    viewport: f32,
    on: &OnAct,
    cx: &App,
) -> impl IntoElement + use<> {
    let members = m.tab == Tab::Members;
    let narrow = viewport <= 620.;
    let body = if members {
        div()
            .flex_1()
            .min_h_0()
            .flex()
            .flex_col()
            .child(stats(m))
            .child(div().flex_shrink_0().mx(px(18.)).mt(px(12.)).text_size(px(12.)).line_height(px(18.)).text_color(white_a(0.44)).child("Everyone is kept by default. Untick someone to cut them off at the next rotation."))
            .child(
                div()
                    .flex_shrink_0()
                    .px(px(18.))
                    .py(px(12.))
                    .flex()
                    .items_center()
                    .gap(px(10.))
                    .child(
                        div()
                            .flex_1()
                            .min_w(px(220.))
                            .h(px(36.))
                            .relative()
                            .flex()
                            .items_center()
                            .pl(px(36.))
                            .pr(px(12.))
                            .rounded(px(9.))
                            .border_1()
                            .border_color(ca(57, 57, 57, 0.6))
                            .bg(white_a(0.03))
                            .overflow_hidden()
                            .child(svg().path("icons/search.svg").absolute().left(px(12.)).top(px(10.)).size(px(15.)).text_color(white_a(0.35)))
                            .child(div().flex_1().min_w_0().child(m.query.clone())),
                    )
                    .child(chips(m, on)),
            )
            .child(div().id("mod-list").flex_1().min_h_0().overflow_y_scroll().track_scroll(&m.scroll).px(px(12.)).pb(px(10.)).child(list(m, store, on, cx)))
            .child(foot(m, narrow, on))
            .into_any_element()
    } else {
        crate::policies::view(
            &m.policies,
            m,
            store,
            (if narrow { viewport } else { (viewport - 52.).min(1040.) }) - 38.,
            on,
            cx,
        )
    };
    let close = on.clone();
    gpui::deferred(
        div()
            .id("mod-overlay")
            .absolute()
            .inset_0()
            .bg(ca(0, 0, 0, 0.88))
            .flex()
            .items_center()
            .justify_center()
            .when(!narrow, |d| d.p(px(26.)))
            .occlude()
            .on_click(move |_, w, cx| close(Act::Close, w, cx))
            .child(
                div()
                    .id("mod-card")
                    .w_full()
                    .max_h_full()
                    .min_h(px(300.))
                    .flex()
                    .flex_col()
                    .when(narrow, |d| d.h_full())
                    .when(!narrow, |d| d.max_w(px(1040.)).rounded(px(18.)).border_1().border_color(c(0x2c2e2f)))
                    .bg(c(0x101010))
                    .overflow_hidden()
                    .font_family(INTER)
                    .occlude()
                    .on_click(|_, _, cx| cx.stop_propagation())
                    .child(head(m, on))
                    .children(alert(m))
                    .child(tabs(m, on))
                    .child(body),
            ),
    )
    .with_priority(12)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ages() {
        assert_eq!(ago(40), "40s");
        assert_eq!(ago(240), "4m");
        assert_eq!(ago(86400 * 2), "2d");
    }
}
