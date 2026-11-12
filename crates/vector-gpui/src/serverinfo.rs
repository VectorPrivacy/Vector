//! The media server dialog's body: `BlossomPerformance.svelte` (this device's latency and
//! upload speeds, the last uploads as a curve), `BlossomAccount.svelte` (the plan the server
//! puts this account on) and `BlossomCaps.svelte` (what uploads have taught us it takes).
//! Data arrives as the backend serialises it, so fixtures and live read one shape.

use std::cell::Cell;
use std::rc::Rc;

use gpui::prelude::*;
use gpui::{AnyElement, App, Bounds, FontWeight, Pixels, SharedString, Window, div, point, px};
use serde_json::Value;

use crate::media::format_bytes_with;
use crate::theme::*;
use crate::timeline::DANGER;

pub enum Load<T> {
    Loading,
    Ok(T),
    Error,
}

/// The chart's hovered upload, and where its tip is drawn while it eases there.
#[derive(Default)]
pub struct Chart {
    pub hover: Option<usize>,
    pub tip: Cell<Option<(f32, f32)>>,
    pub bounds: Rc<Cell<Option<Bounds<Pixels>>>>,
}

pub type OnHover = Rc<dyn Fn(Option<usize>, &mut Window, &mut App)>;

const W: f32 = 300.;
const H: f32 = 44.;
const PAD: f32 = 4.;

fn bytes(n: f64, d: usize) -> String {
    format_bytes_with(n, d, false)
}

fn mbit(v: f64) -> String {
    if v >= 10. { format!("{v:.0}") } else { format!("{v:.1}") }
}

fn label(text: &str) -> gpui::Div {
    div().text_size(px(11.)).line_height(px(24.)).text_color(white_a(0.5)).child(text.to_uppercase())
}

fn samples(stats: &Value) -> Vec<(f64, u64)> {
    stats["recent"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|s| match s.as_array() {
            Some(a) => (a.first().and_then(Value::as_f64).unwrap_or(0.), a.get(1).and_then(Value::as_u64).unwrap_or(0)),
            None => (s.as_f64().unwrap_or(0.), 0),
        })
        .collect()
}

/// The points in viewBox units: x across 0..W, y down 0..H.
fn points(s: &[(f64, u64)]) -> Vec<(f32, f32)> {
    if s.len() < 2 {
        return Vec::new();
    }
    let scale = s.iter().map(|p| p.0).fold(0.001, f64::max) as f32;
    let n = (s.len() - 1) as f32;
    s.iter()
        .enumerate()
        .map(|(i, p)| (PAD + i as f32 / n * (W - PAD * 2.), H - PAD - p.0 as f32 / scale * (H - PAD * 2.)))
        .collect()
}

pub fn performance(stats: &Value, chart: &Chart, window: &mut Window, on_hover: OnHover) -> Option<AnyElement> {
    let latency = stats["latency_ms"].as_f64();
    let mbps = stats["mbps"].as_f64();
    if latency.is_none() && mbps.is_none() {
        return None;
    }
    let stat = |name: &str, value: Option<String>, unit: &'static str| {
        div()
            .flex_1()
            .min_w_0()
            .flex()
            .flex_col()
            .gap(px(2.))
            .px(px(10.))
            .py(px(8.))
            .rounded(px(10.))
            .bg(white_a(0.03))
            .border_1()
            .border_color(white_a(0.06))
            .child(label(name))
            .child(
                div()
                    .h(px(26.))
                    .flex()
                    .items_baseline()
                    .gap(px(4.))
                    .text_size(px(16.))
                    .line_height(px(24.))
                    .font_weight(FontWeight::BOLD)
                    .text_color(c(0xffffff))
                    .map(|d| match value {
                        Some(v) => d.child(v).child(div().text_size(px(11.)).font_weight(FontWeight::NORMAL).text_color(white_a(0.45)).child(unit)),
                        None => d.child("–"),
                    }),
            )
    };
    let s = samples(stats);
    let pts = points(&s);
    let uploads = stats["uploads"].as_u64().unwrap_or(0);
    let total = stats["bytes_total"].as_u64().unwrap_or(0);
    let mut foot = if pts.is_empty() {
        format!("{uploads} upload{}", if uploads == 1 { "" } else { "s" })
    } else {
        format!("Speed of your last {} uploads", s.len())
    };
    if total > 0 {
        foot = format!("{foot} · {}", bytes(total as f64, 1));
    }
    let chart_el = (!pts.is_empty()).then(|| spark(&s, &pts, chart, window, on_hover));
    Some(
        div()
            .flex()
            .flex_col()
            .child(
                div()
                    .flex()
                    .gap(px(10.))
                    .child(stat("Latency", latency.map(|l| format!("{}", l.round() as i64)), "ms"))
                    .child(stat("Upload", mbps.map(mbit), "Mbit/s"))
                    .child(stat("Best", stats["best_mbps"].as_f64().map(mbit), "Mbit/s")),
            )
            .children(chart_el)
            .child(div().mt(px(4.)).text_size(px(11.)).line_height(px(24.)).text_color(white_a(0.45)).child(foot))
            .into_any_element(),
    )
}

fn spark(s: &[(f64, u64)], pts: &[(f32, f32)], chart: &Chart, window: &mut Window, on_hover: OnHover) -> AnyElement {
    let pts: Rc<[(f32, f32)]> = pts.into();
    let hover = chart.hover.filter(|&i| i < pts.len());
    // The tip closes a fixed share of the way to its dot each frame.
    let tip = hover.map(|i| {
        let goal = pts[i];
        let at = match chart.tip.get() {
            Some((x, y)) if (goal.0 - x).abs() >= 0.05 || (goal.1 - y).abs() >= 0.05 => {
                window.request_animation_frame();
                (x + (goal.0 - x) * 0.22, y + (goal.1 - y) * 0.22)
            }
            _ => goal,
        };
        chart.tip.set(Some(at));
        (i, at)
    });
    if hover.is_none() {
        chart.tip.set(None);
    }
    let paint_pts = pts.clone();
    let probe = chart.bounds.clone();
    let (mv, leave) = (on_hover.clone(), on_hover);
    let bounds = chart.bounds.clone();
    let stats_pts = pts.clone();
    let tip_el = tip.map(|(i, (x, y))| {
        let (mbps, size) = s[i];
        let side = if x < W * 0.2 { -1 } else if x > W * 0.8 { 1 } else { 0 };
        let text = div()
            .flex_shrink_0()
            .px(px(7.))
            .py(px(3.))
            .rounded(px(6.))
            .bg(ca(20, 20, 20, 0.96))
            .border_1()
            .border_color(white_a(0.12))
            .text_size(px(11.))
            .line_height(px(16.))
            .whitespace_nowrap()
            .flex()
            .child(div().text_color(c(ACCENT)).font_weight(FontWeight::SEMIBOLD).child(format!("{} Mbit/s", mbit(mbps))))
            .when(size > 0, |d| d.child(format!(" · {}", bytes(size as f64, 1))));
        // A zero-width anchor at the dot; the tip hangs from it, inward near an edge.
        div()
            .absolute()
            .left(gpui::relative(x / W))
            .top(px(y - 8.))
            .w_0()
            .h_0()
            .flex()
            .items_end()
            .map(|d| match side {
                -1 => d.justify_start().ml(px(-4.)),
                1 => d.justify_end().ml(px(4.)),
                _ => d.justify_center(),
            })
            .child(text)
    });
    div()
        .id("blossom-perf-chart")
        .relative()
        .mt(px(8.))
        .h(px(H))
        .on_mouse_move(move |e, w, cx| {
            let Some(b) = bounds.get() else { return };
            let x = f32::from(e.position.x - b.left());
            let width = f32::from(b.size.width);
            let vx = x / width.max(1.) * W;
            let i = stats_pts.iter().enumerate().min_by(|a, b| (a.1.0 - vx).abs().total_cmp(&(b.1.0 - vx).abs())).map(|(i, _)| i);
            if i != hover {
                mv(i, w, cx)
            }
        })
        .on_hover(move |h, w, cx| {
            if !*h {
                leave(None, w, cx)
            }
        })
        .child(
            gpui::canvas(
                move |b, _, _| probe.set(Some(b)),
                move |b, _, window, _| {
                    let sx = f32::from(b.size.width) / W;
                    let at = |p: (f32, f32)| point(b.left() + px(p.0 * sx), b.top() + px(p.1));
                    let p = &paint_pts;
                    let mut curve = gpui::PathBuilder::stroke(px(1.5));
                    let mut area = gpui::PathBuilder::fill();
                    curve.move_to(at(p[0]));
                    area.move_to(at(p[0]));
                    // Catmull-Rom through every point as cubic Béziers.
                    for i in 0..p.len() - 1 {
                        let (p0, p1, p2) = (p[i.saturating_sub(1)], p[i], p[i + 1]);
                        let p3 = p.get(i + 2).copied().unwrap_or(p2);
                        let c1 = (p1.0 + (p2.0 - p0.0) / 6., p1.1 + (p2.1 - p0.1) / 6.);
                        let c2 = (p2.0 - (p3.0 - p1.0) / 6., p2.1 - (p3.1 - p1.1) / 6.);
                        curve.cubic_bezier_to(at(p2), at(c1), at(c2));
                        area.cubic_bezier_to(at(p2), at(c1), at(c2));
                    }
                    area.line_to(at((p[p.len() - 1].0, H)));
                    area.line_to(at((p[0].0, H)));
                    area.close();
                    if let Ok(path) = area.build() {
                        window.paint_path(
                            path,
                            gpui::linear_gradient(
                                180.,
                                gpui::linear_color_stop(ca(0x59, 0xfc, 0xb3, 0.22), 0.),
                                gpui::linear_color_stop(ca(0x59, 0xfc, 0xb3, 0.), 1.),
                            ),
                        );
                    }
                    if let Ok(path) = curve.build() {
                        window.paint_path(path, ca(0x59, 0xfc, 0xb3, 0.75));
                    }
                    for (i, q) in p.iter().enumerate() {
                        let d = if hover == Some(i) {
                            7.
                        } else if i + 1 == p.len() {
                            6.
                        } else {
                            4.
                        };
                        let c = at(*q);
                        window.paint_quad(gpui::fill(
                            Bounds::new(point(c.x - px(d / 2.), c.y - px(d / 2.)), gpui::size(px(d), px(d))),
                            c_accent(),
                        ).corner_radii(px(d / 2.)));
                    }
                },
            )
            .absolute()
            .size_full(),
        )
        .children(tip_el)
        .into_any_element()
}

fn c_accent() -> gpui::Hsla {
    c(ACCENT)
}

fn meter(name: &str, used: u64, limit: u64, note: String) -> gpui::Div {
    let pct = if limit == 0 { 0. } else { (used as f32 / limit as f32).min(1.) };
    let fill = if limit > 0 && used >= limit {
        gpui::Background::from(c(DANGER))
    } else if limit > 0 && pct >= 0.9 {
        gpui::linear_gradient(90., gpui::linear_color_stop(c(0xffc107), 0.), gpui::linear_color_stop(c(0xff9800), 1.))
    } else {
        gpui::linear_gradient(90., gpui::linear_color_stop(c(PRIMARY), 0.), gpui::linear_color_stop(c(ACCENT), 1.))
    };
    let pct = (pct * 100.).round() / 100.;
    div()
        .flex()
        .flex_col()
        .gap(px(6.))
        .child(
            div()
                .h(px(25.))
                .flex()
                .items_baseline()
                .justify_between()
                .gap(px(8.))
                .child(
                    div()
                        .min_w_0()
                        .flex()
                        .overflow_hidden()
                        .whitespace_nowrap()
                        .text_size(px(11.))
                        .line_height(px(24.))
                        .text_color(white_a(0.5))
                        .child(name.to_uppercase())
                        .child(div().text_color(white_a(0.35)).child(format!(" · {note}"))),
                )
                .child(
                    div()
                        .flex_shrink_0()
                        .flex()
                        .gap(px(4.))
                        .text_size(px(13.))
                        .line_height(px(24.))
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(c(0xffffff))
                        .child(bytes(used as f64, 1))
                        .child(div().font_weight(FontWeight::NORMAL).text_color(white_a(0.45)).child(format!("of {}", bytes(limit as f64, 1)))),
                ),
        )
        .child(
            div()
                .h(px(6.))
                .w_full()
                .rounded(px(3.))
                .bg(white_a(0.08))
                .overflow_hidden()
                .child(div().h_full().min_w(px(3.)).w(gpui::relative(pct)).rounded(px(3.)).bg(fill)),
        )
}

const OFFICIAL: [&str; 2] = ["vectorapp.io", "jskitty.com"];

/// `BlossomAccount.svelte`; `None` when the document says nothing for this account.
pub fn account(info: &Value, host: &str) -> Option<AnyElement> {
    let caller = &info["caller"];
    if !caller.is_object() {
        let max = info["max_blob"].as_u64()?;
        return Some(blurb(format!("Accepts files up to {}.", bytes(max as f64, 1))).into_any_element());
    }
    let allowed = caller["allowed"].as_bool().unwrap_or(false);
    let official = OFFICIAL.iter().any(|d| host == *d || host.ends_with(&format!(".{d}")));
    let tier = caller["tier"].as_str().unwrap_or_default();
    let premium = allowed && official && tier == "badged";
    let name = if !allowed {
        "No access".to_string()
    } else {
        caller["plan_title"].as_str().filter(|t| !t.is_empty()).or(Some(tier).filter(|t| !t.is_empty())).unwrap_or("Member").to_string()
    };
    let desc = if allowed { caller["plan_description"].as_str().unwrap_or_default() } else { "" };
    let n = |k: &str| caller[k].as_u64().unwrap_or(0);
    let title = div()
        .flex()
        .flex_col()
        .gap(px(2.))
        .min_w_0()
        .child(label("Your plan"))
        .child(
            div()
                .text_size(px(22.))
                .line_height(px(24.2))
                .font_weight(FontWeight::BOLD)
                .text_color(if !allowed {
                    c(DANGER)
                } else if premium {
                    c(0x46eba6)
                } else {
                    c(0xffffff)
                })
                .child(name),
        )
        .when(!desc.is_empty(), |d| {
            d.child(div().mt(px(4.)).text_size(px(12.)).line_height(px(16.8)).text_color(ca(246, 246, 246, 0.55)).child(desc.to_string()))
        });
    let head = div()
        .flex()
        .items_start()
        .justify_between()
        .gap(px(12.))
        .child(title)
        .when(allowed, |d| {
            d.child(
                div()
                    .flex_shrink_0()
                    .flex()
                    .flex_col()
                    .items_end()
                    .child(
                        div()
                            .text_size(px(22.))
                            .line_height(px(24.2))
                            .font_weight(FontWeight::BOLD)
                            .whitespace_nowrap()
                            .text_color(c(ACCENT))
                            .child(bytes(n("max_blob") as f64, 0)),
                    )
                    .child(label("per file").mt(px(3.))),
            )
        });
    let body = if allowed {
        let files = n("blobs");
        div()
            .flex()
            .flex_col()
            .gap(px(10.))
            .child(meter("Storage", n("storage_used"), n("storage_limit"), format!("{files} file{}", if files == 1 { "" } else { "s" })))
            .child(meter("Today", n("daily_used"), n("daily_limit"), "resets 00:00 UTC".into()))
    } else {
        let reasons: Vec<&str> = caller["reasons"].as_array().into_iter().flatten().filter_map(Value::as_str).collect();
        let text = if reasons.is_empty() { "This server won’t take uploads from your account.".to_string() } else { reasons.join(" ") };
        div().text_size(px(13.)).line_height(px(18.85)).text_color(white_a(0.75)).child(text)
    };
    let card = div()
        .flex()
        .flex_col()
        .gap(px(12.))
        .pt(px(12.))
        .px(px(14.))
        .pb(px(14.))
        .rounded(px(12.))
        .child(head)
        .child(body);
    // The premium card's border is a gradient: a 1px gradient frame around the card.
    Some(if premium {
        div()
            .p(px(1.))
            .rounded(px(12.))
            .bg(gpui::linear_gradient(135., gpui::linear_color_stop(c(PRIMARY), 0.), gpui::linear_color_stop(c(ACCENT), 1.)))
            .child(card.rounded(px(11.)).bg(ca(20, 20, 20, 0.98)))
            .into_any_element()
    } else {
        card.bg(white_a(0.03))
            .border_1()
            .border_color(if allowed { white_a(0.08) } else { ca(252, 89, 92, 0.35) })
            .into_any_element()
    })
}

/// The plan card's shape while the document loads, so its answer changes text, not geometry.
pub fn skeleton() -> AnyElement {
    let bar = |w: f32| div().w(px(w)).h(px(24.2)).rounded(px(6.)).bg(white_a(0.06));
    let m = |name: &str| {
        div()
            .flex()
            .flex_col()
            .gap(px(6.))
            .child(div().h(px(25.)).flex().justify_between().child(label(name)).child(div().w(px(52.)).h(px(24.)).rounded(px(6.)).bg(white_a(0.06))))
            .child(div().h(px(6.)).rounded(px(3.)).bg(white_a(0.08)))
    };
    div()
        .opacity(0.5)
        .flex()
        .flex_col()
        .gap(px(12.))
        .pt(px(12.))
        .px(px(14.))
        .pb(px(14.))
        .rounded(px(12.))
        .bg(white_a(0.03))
        .border_1()
        .border_color(white_a(0.08))
        .child(
            div()
                .flex()
                .justify_between()
                .child(div().flex().flex_col().gap(px(2.)).child(label("Your plan")).child(bar(70.)))
                .child(div().flex().flex_col().items_end().child(bar(70.)).child(label("per file").mt(px(3.)))),
        )
        .child(div().flex().flex_col().gap(px(10.)).child(m("Storage")).child(m("Today")))
        .into_any_element()
}

fn blurb(text: impl Into<SharedString>) -> gpui::Div {
    div().text_size(px(12.)).line_height(px(17.4)).text_color(ca(246, 246, 246, 0.55)).child(text.into())
}

/// `BlossomCaps.svelte` under its "What we’ve learned" header.
pub fn learned(caps: &Load<Vec<Value>>) -> AnyElement {
    let body: AnyElement = match caps {
        Load::Loading => quiet("Loading…"),
        Load::Error => quiet("Could not load capability data."),
        Load::Ok(list) if list.is_empty() => {
            quiet("No capability data yet. Vector learns each server’s file-type and size limits as you send files.")
        }
        Load::Ok(list) => {
            let o = |c: &Value| c["outcome"].as_u64().unwrap_or(0);
            let max = |c: &Value| c["max_accepted_size"].as_u64().unwrap_or(0);
            let min_rej = |c: &Value| c["min_rejected_size"].as_u64();
            let accepted: Vec<&Value> = list.iter().filter(|c| o(c) == 1 && max(c) > 0).collect();
            let limited: Vec<&Value> = list
                .iter()
                .filter(|c| (o(c) == 3 && min_rej(c).is_some()) || (o(c) == 1 && max(c) == 0 && min_rej(c).is_some()))
                .collect();
            let rejected: Vec<&Value> = list.iter().filter(|c| o(c) == 2).collect();
            let mut col = div().flex().flex_col();
            // The first label's top margin collapses into the blurb's.
            let mut first = true;
            let mut group = |name: &str, tint: u32| {
                let top = if std::mem::take(&mut first) { 0. } else { 8. };
                div()
                    .mt(px(top))
                    .mb(px(4.))
                    .text_size(px(11.))
                    .line_height(px(24.))
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(c(tint).opacity(0.7))
                    .child(name.to_uppercase())
            };
            let rows = |items: &[&Value], kind: u8| {
                let n = items.len();
                div().flex().flex_col().children(items.iter().enumerate().map(move |(i, cap)| row(cap, kind, i + 1 == n)))
            };
            if !accepted.is_empty() {
                col = col.child(group("Accepts", ACCENT)).child(rows(&accepted, 1));
            }
            if !limited.is_empty() {
                col = col.child(group("Size-limited", 0xffc107)).child(rows(&limited, 3));
            }
            if !rejected.is_empty() {
                col = col.child(group("Rejects", DANGER)).child(rows(&rejected, 2));
            }
            col.into_any_element()
        }
    };
    div()
        .flex()
        .flex_col()
        .child(
            div()
                .text_size(px(13.))
                .line_height(px(24.))
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(white_a(0.7))
                .child("What we’ve learned"),
        )
        .child(blurb("This server doesn’t say what it accepts, so Vector learns as you send: the largest file it has taken of each type, and the types it has refused. Uploads are routed to the best-suited server automatically.").mt(px(8.)).mb(px(12.)))
        .child(body)
        .into_any_element()
}

fn quiet(text: &'static str) -> AnyElement {
    div().text_size(px(13.)).line_height(px(24.)).text_color(ca(246, 246, 246, 0.51)).child(text).into_any_element()
}

fn row(cap: &Value, kind: u8, last: bool) -> impl IntoElement {
    let mime = cap["mime_type"].as_str().unwrap_or_default().to_string();
    let enc = cap["is_encrypted"].as_bool().unwrap_or(false);
    let marker = |glyph: &'static str, tint: u32| {
        div().w(px(14.)).flex_shrink_0().flex().justify_center().text_size(px(12.)).font_weight(FontWeight::BOLD).text_color(c(tint)).child(glyph)
    };
    let context = div()
        .flex_shrink_0()
        .px(px(5.))
        .py(px(2.))
        .rounded(px(3.))
        .bg(white_a(0.08))
        .text_size(px(9.))
        .line_height(px(24.))
        .font_weight(FontWeight::SEMIBOLD)
        .text_color(white_a(0.55))
        .child(if enc { "ENCRYPTED" } else { "PUBLIC" });
    let size = |t: String| div().flex_shrink_0().text_size(px(11.)).line_height(px(24.)).text_color(ca(246, 246, 246, 0.425)).child(t);
    let fade = match kind {
        2 => 0.5,
        3 => 0.75,
        _ => 0.85,
    };
    let mime_el = div()
        .flex_1()
        .min_w_0()
        .font_family(crate::markdown::MONO)
        .text_size(px(12.))
        .line_height(px(24.))
        .text_color(ca(246, 246, 246, 0.85 * fade))
        .child(mime);
    div()
        .flex()
        .items_center()
        .gap(px(8.))
        .py(px(4.))
        .when(!last, |d| d.border_b_1().border_color(white_a(0.05)))
        .map(|d| match kind {
            1 => d
                .child(marker("✓", ACCENT))
                .child(mime_el)
                .child(context)
                .child(size(format!("{} max", bytes(cap["max_accepted_size"].as_f64().unwrap_or(0.), 1)))),
            3 => d
                .child(marker("⚠", 0xffc107))
                .child(mime_el)
                .child(context)
                .child(size(format!("rejects ≥ {}", bytes(cap["min_rejected_size"].as_f64().unwrap_or(0.), 1)))),
            _ => d.child(mime_el).child(context).child(marker("✕", DANGER)),
        })
}
