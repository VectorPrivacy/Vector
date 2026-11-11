//! The Network section's dialogs (`settings/network/`): Add Relay, one relay's settings and
//! activity, and one media server.

use std::rc::Rc;

use gpui::prelude::*;
use gpui::{AnyElement, App, Entity, FontWeight, SharedString, Window, div, px, svg};

use crate::backend::network::{RelayLog, RelayMetrics, RelayRow};
use crate::composer::TextInput;
use crate::kit::Choice;
use crate::theme::*;
use crate::timeline::DANGER;

pub enum NetDialog {
    AddRelay {
        input: Entity<TextInput>,
        mode: &'static str,
    },
    Relay {
        row: RelayRow,
        metrics: RelayMetrics,
        logs: Vec<RelayLog>,
        mode_open: bool,
        copied: bool,
    },
    Server {
        info: vector_core::blossom_servers::BlossomServerInfo,
    },
}

pub enum Act {
    Close,
    /// Add Relay: the mode chosen, then Add.
    Mode(&'static str),
    Add,
    /// The relay dialog: its mode list, a mode, Disable/Enable/Remove, copy the log.
    ToggleModeList,
    RelayMode(&'static str),
    RelayAction,
    CopyLogs,
    ServerAction,
}

pub type OnAct = Rc<dyn Fn(Act, &mut Window, &mut App)>;

const ADD_MODES: [(&str, &str); 3] = [
    ("both", "Read & Write"),
    ("read", "Read"),
    ("write", "Write"),
];
const RELAY_MODES: [Choice; 3] = [
    Choice {
        value: "both",
        label: "Read & Write",
    },
    Choice {
        value: "read",
        label: "Read Only",
    },
    Choice {
        value: "write",
        label: "Write Only",
    },
];

fn click(
    on: &OnAct,
    act: impl Fn() -> Act + 'static,
) -> impl Fn(&gpui::ClickEvent, &mut Window, &mut App) + 'static {
    let on = on.clone();
    move |_, w, cx| on(act(), w, cx)
}

/// `.relay-status.relay-status-small`: the socket state as a pill.
fn pill(status: &str) -> AnyElement {
    let (fg, bg, border) = match status {
        "connected" => (c(0x59fcb3), ca(90, 252, 180, 0.1), ca(90, 252, 180, 0.5)),
        "connecting" => (c(0xffc107), ca(255, 193, 7, 0.2), ca(255, 193, 7, 0.3)),
        "disconnected" => (c(0xff5252), ca(255, 82, 82, 0.2), ca(255, 82, 82, 0.3)),
        "pending" => (c(0x9c27b0), ca(156, 39, 176, 0.2), ca(156, 39, 176, 0.3)),
        "sleeping" => (c(0x6495ed), ca(100, 149, 237, 0.2), ca(100, 149, 237, 0.3)),
        "disabled" => (c(0x666666), ca(128, 128, 128, 0.15), ca(128, 128, 128, 0.2)),
        _ => (c(0x808080), ca(128, 128, 128, 0.2), ca(128, 128, 128, 0.3)),
    };
    div()
        .flex_shrink_0()
        .h(px(30.))
        .px(px(8.))
        .flex()
        .items_center()
        .rounded(px(20.))
        .border_1()
        .border_color(border)
        .bg(bg)
        .text_size(px(10.))
        .font_weight(FontWeight::SEMIBOLD)
        .text_color(fg)
        .child(crate::kit::spaced(status.to_uppercase(), 0.5))
        .into_any_element()
}

/// `.relay-dialog-buttons .btn`.
fn button(id: &'static str, h: f32) -> gpui::Stateful<gpui::Div> {
    div()
        .id(id)
        .flex_1()
        .h(px(h))
        .flex()
        .items_center()
        .justify_center()
        .gap(px(6.))
        .rounded(px(8.))
        .text_size(px(16.))
        .font_weight(FontWeight::SEMIBOLD)
        .text_color(c(0xffffff))
        .cursor_pointer()
}

fn card(id: &'static str, body: impl IntoElement, on: &OnAct) -> AnyElement {
    let close = on.clone();
    gpui::deferred(
        div()
            .id("net-dialog-overlay")
            .absolute()
            .inset_0()
            .flex()
            .items_center()
            .justify_center()
            .bg(ca(0, 0, 0, 0.85))
            .occlude()
            .on_click(move |_, w, cx| close(Act::Close, w, cx))
            .child(
                div()
                    .id(id)
                    .w(px(422.))
                    .rounded(px(16.))
                    .border_1()
                    .border_color(c(0x262626))
                    .bg(ca(20, 20, 20, 0.95))
                    .overflow_hidden()
                    .font_family(INTER)
                    .shadow(vec![gpui::BoxShadow {
                        color: ca(0, 0, 0, 0.4),
                        offset: gpui::point(px(0.), px(8.)),
                        blur_radius: px(32.),
                        spread_radius: px(0.),
                        inset: false,
                    }])
                    // A click on the card stays in it.
                    .on_click(|_, _, cx| cx.stop_propagation())
                    .child(body),
            ),
    )
    .with_priority(9)
    .into_any_element()
}

fn add_relay(input: &Entity<TextInput>, mode: &'static str, on: &OnAct, cx: &App) -> AnyElement {
    let host = input.read(cx).text().trim().to_lowercase();
    let host = host
        .trim_start_matches("wss://")
        .trim_start_matches("ws://")
        .split(['/', ':', '?', '#'])
        .next()
        .unwrap_or("")
        .to_string();
    let inside = if host.ends_with(".i2p") {
        Some("I2P")
    } else if host.ends_with(".onion") {
        Some("Tor")
    } else {
        None
    };
    let header = div()
        .h(px(63.))
        .px(px(20.))
        .flex()
        .items_center()
        .justify_between()
        .bg(c(0x262626))
        .border_b_1()
        .border_color(c(0x333333))
        .child(
            div()
                .text_size(px(18.))
                .line_height(px(24.))
                .font_weight(FontWeight::BOLD)
                .text_color(c(0xffffff))
                .child("Add Relay"),
        )
        .child(
            div()
                .id("add-relay-close")
                .size(px(32.))
                .flex()
                .items_center()
                .justify_center()
                .rounded(px(6.))
                .text_size(px(24.))
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(white_a(0.6))
                .hover(|d| d.bg(white_a(0.1)).text_color(c(0xffffff)))
                .cursor_pointer()
                .on_click(click(on, || Act::Close))
                .child("×"),
        );
    let modes = div()
        .flex()
        .p(px(3.))
        .gap(px(3.))
        .rounded(px(10.))
        .bg(white_a(0.05))
        .children(ADD_MODES.iter().map(|(value, label)| {
            let on_it = *value == mode;
            let v = *value;
            div()
                .id(SharedString::from(format!("add-mode-{value}")))
                .flex_1()
                .h(px(40.))
                .flex()
                .items_center()
                .justify_center()
                .rounded(px(8.))
                .text_size(px(13.))
                .when(on_it, |d| d.bg(white_a(0.12)).text_color(c(0xffffff)))
                .when(!on_it, |d| d.text_color(white_a(0.6)))
                .cursor_pointer()
                .on_click(click(on, move || Act::Mode(v)))
                .child(*label)
        }));
    let mut body = div()
        .flex()
        .flex_col()
        .gap(px(12.))
        .pt(px(18.))
        .px(px(20.))
        .pb(px(20.))
        .child(
            div()
                .h(px(42.))
                .px(px(12.))
                .flex()
                .items_center()
                .rounded(px(8.))
                .border_1()
                .border_color(c(0x262626))
                .bg(ca(0, 0, 0, 0.3))
                .child(input.clone()),
        );
    if let Some(net) = inside {
        body = body.child(
            div()
                .text_size(px(12.))
                .text_color(white_a(0.5))
                .child(format!(
                    "Only people using {net} can send to you on this relay."
                )),
        );
    }
    body = body.child(modes).child(
        div()
            .mt(px(4.))
            .flex()
            .gap(px(10.))
            .child(
                button("add-relay-cancel", 42.)
                    .border_1()
                    .border_color(white_a(0.2))
                    .hover(|d| d.opacity(0.75))
                    .on_click(click(on, || Act::Close))
                    .child("Cancel"),
            )
            .child(
                button("add-relay-confirm", 42.)
                    .bg(c(0x59fcb3))
                    .text_color(c(0x000000))
                    .hover(|d| d.opacity(0.75))
                    .on_click(click(on, || Act::Add))
                    .child("Add"),
            ),
    );
    card("add-relay", div().child(header).child(body), on)
}

/// `.relay-form-select`: a native select's look, with its list under it.
fn native_select(value: &str, disabled: bool, open: bool, on: &OnAct) -> AnyElement {
    let label = RELAY_MODES
        .iter()
        .find(|m| m.value == value)
        .map(|m| m.label)
        .unwrap_or("");
    let chevron = svg()
        .path("icons/chevron-down.svg")
        .size(px(12.))
        .text_color(white_a(0.6));
    let mut field = div()
        .id("relay-info-mode")
        .relative()
        .w(px(160.))
        .h(px(29.))
        .pl(px(14.))
        .pr(px(8.))
        .flex()
        .items_center()
        .justify_between()
        .rounded(px(8.))
        .border_1()
        .border_color(if open { c(PRIMARY) } else { c(0x262626) })
        .bg(ca(0, 0, 0, 0.3))
        .text_size(px(13.))
        .text_color(c(0xffffff))
        .child(label)
        .child(chevron);
    if disabled {
        return field.opacity(0.5).into_any_element();
    }
    field = field
        .cursor_pointer()
        .on_click(click(on, || Act::ToggleModeList));
    if open {
        field = field.child(
            gpui::deferred(
                div()
                    .absolute()
                    .top(px(31.))
                    .left_0()
                    .w(px(160.))
                    .p(px(4.))
                    .rounded(px(8.))
                    .border_1()
                    .border_color(c(0x2a2a2a))
                    .bg(c(0x111111))
                    .occlude()
                    .children(RELAY_MODES.iter().map(|m| {
                        let v = m.value;
                        let on_it = v == value;
                        div()
                            .id(SharedString::from(format!("mode-{v}")))
                            .h(px(28.))
                            .px(px(10.))
                            .flex()
                            .items_center()
                            .rounded(px(6.))
                            .text_size(px(13.))
                            .text_color(if on_it { c(0xffffff) } else { c(0xcfcfcf) })
                            .when(on_it, |d| d.bg(c(0x1f1f1f)))
                            .hover(|d| d.bg(c(0x1f1f1f)))
                            .cursor_pointer()
                            .on_click(click(on, move || Act::RelayMode(v)))
                            .child(m.label)
                    })),
            )
            .with_priority(11),
        );
    }
    field.into_any_element()
}

fn ago(secs: u64) -> String {
    let now = vector_core::clock::now_secs();
    let diff = now.saturating_sub(secs);
    if diff < 60 {
        format!("{diff}s ago")
    } else if diff < 3600 {
        format!("{}m ago", diff / 60)
    } else {
        chrono::DateTime::from_timestamp(secs as i64, 0)
            .map(|t| {
                t.with_timezone(&chrono::Local)
                    .format("%-I:%M:%S %p")
                    .to_string()
            })
            .unwrap_or_default()
    }
}

fn relay(
    row: &RelayRow,
    metrics: &RelayMetrics,
    logs: &[RelayLog],
    mode_open: bool,
    copied: bool,
    on: &OnAct,
) -> AnyElement {
    let status = if row.enabled { row.status } else { "disabled" };
    let ping_color = match metrics.ping_ms {
        None => c(0x59fcb3),
        Some(ms) if ms < 200 => c(0x59fcb3),
        Some(ms) if ms < 500 => c(0xfce459),
        Some(ms) if ms < 1000 => c(0xfca559),
        Some(_) => c(DANGER),
    };
    let ping = metrics
        .ping_ms
        .map(|ms| format!("{ms}ms"))
        .unwrap_or_else(|| "--".into());
    let checked = metrics.last_check.map(ago).unwrap_or_else(|| "--".into());
    let metric_label = |t: &'static str| {
        div()
            .text_size(px(11.))
            .line_height(px(24.))
            .text_color(white_a(0.5))
            .child(crate::kit::spaced(t.to_uppercase(), 0.5))
    };
    let metric_value = |t: String, color| {
        div()
            .text_size(px(13.))
            .line_height(px(24.))
            .font_weight(FontWeight::SEMIBOLD)
            .text_color(color)
            .child(t)
    };
    let mode = native_select(&row.mode, row.is_default, mode_open, on);
    let log_list: AnyElement = if logs.is_empty() {
        div()
            .py(px(10.))
            .text_center()
            .text_size(px(12.))
            .line_height(px(24.))
            .italic()
            .text_color(white_a(0.7))
            .child("No activity recorded yet")
            .into_any_element()
    } else {
        div()
            .id("relay-logs")
            .max_h(px(150.))
            .overflow_y_scroll()
            .children(logs.iter().enumerate().map(|(i, l)| {
                let color = match l.level.as_str() {
                    "warn" => c(0xffc107),
                    "error" => c(0xff5252),
                    _ => white_a(0.7),
                };
                let at = chrono::DateTime::from_timestamp(l.timestamp as i64, 0)
                    .map(|t| {
                        t.with_timezone(&chrono::Local)
                            .format("%-I:%M:%S %p")
                            .to_string()
                    })
                    .unwrap_or_default();
                div()
                    .px(px(12.))
                    .py(px(6.))
                    .when(i + 1 < logs.len(), |d| {
                        d.border_b_1().border_color(white_a(0.05))
                    })
                    .text_size(px(12.))
                    .child(
                        div()
                            .flex()
                            .child(
                                div()
                                    .mr(px(8.))
                                    .text_size(px(10.))
                                    .text_color(white_a(0.4))
                                    .child(at),
                            )
                            .child(div().text_color(color).child(l.message.clone())),
                    )
            }))
            .into_any_element()
    };
    let action = if row.is_default && !row.enabled {
        "Enable"
    } else {
        "Disable"
    };
    let body = div()
        .p(px(20.))
        .flex()
        .flex_col()
        .gap(px(15.))
        .child(
            div()
                .flex()
                .items_center()
                .justify_between()
                .gap(px(8.))
                .child(
                    div()
                        .min_w_0()
                        .truncate()
                        .text_size(px(13.))
                        .line_height(px(24.))
                        .text_color(white_a(0.5))
                        .child(crate::settings::host(&row.url)),
                )
                .child(pill(status)),
        )
        .child(
            div()
                .flex()
                .items_center()
                .gap(px(4.))
                .child(metric_label("Ping"))
                .child(metric_value(ping, ping_color))
                .child(div().flex_1())
                .child(metric_label("Last Check"))
                .child(metric_value(checked, c(0x59fcb3))),
        )
        .child(
            div()
                .flex()
                .items_center()
                .justify_between()
                .child(
                    div()
                        .text_size(px(13.))
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(white_a(0.8))
                        .child("Mode"),
                )
                .child(mode),
        )
        .child(
            div()
                .flex()
                .flex_col()
                .child(
                    div()
                        .flex()
                        .items_center()
                        .justify_between()
                        .child(
                            div()
                                .text_size(px(13.))
                                .line_height(px(24.))
                                .font_weight(FontWeight::SEMIBOLD)
                                .text_color(white_a(0.7))
                                .child("Recent Activity"),
                        )
                        .child(
                            div()
                                .id("relay-logs-copy")
                                .size(px(24.))
                                .flex()
                                .items_center()
                                .justify_center()
                                .rounded(px(4.))
                                .hover(|d| d.bg(white_a(0.1)))
                                .cursor_pointer()
                                .on_click(click(on, || Act::CopyLogs))
                                .child(
                                    svg()
                                        .path(if copied {
                                            "icons/check.svg"
                                        } else {
                                            "icons/copy.svg"
                                        })
                                        .size(px(14.))
                                        .text_color(white_a(0.5)),
                                ),
                        ),
                )
                .child(
                    div()
                        .mt(px(10.))
                        .rounded(px(8.))
                        .bg(ca(0, 0, 0, 0.3))
                        .child(log_list),
                ),
        )
        .child(
            div()
                .mt(px(10.))
                .flex()
                .gap(px(10.))
                .child(
                    button("relay-action", 44.)
                        .pt(px(2.))
                        .border_1()
                        .border_color(c(DANGER))
                        .hover(|d| d.opacity(0.75))
                        .on_click(click(on, || Act::RelayAction))
                        .child(
                            svg()
                                .path("icons/disable.svg")
                                .size(px(16.))
                                .mt(px(-2.))
                                .text_color(c(DANGER)),
                        )
                        .child(action),
                )
                .child(
                    button("relay-info-done", 44.)
                        .border_2()
                        .border_color(c(0x474747))
                        .hover(|d| d.opacity(0.75))
                        .on_click(click(on, || Act::Close))
                        .child("Close"),
                ),
        );
    card("relay-info", body, on)
}

fn server(info: &vector_core::blossom_servers::BlossomServerInfo, on: &OnAct) -> AnyElement {
    let state = if info.enabled {
        info.status.state
    } else {
        "disabled"
    };
    let notice = match state {
        "offline" => Some(
            "Vector couldn’t reach this server recently. Uploads are routed elsewhere until it answers again.",
        ),
        "maxed" => Some(
            "An allowance on this server is used up. Uploads are routed elsewhere until it frees up.",
        ),
        "no_access" => Some("This server won’t take uploads from your account."),
        "disabled" => Some("Disabled — uploads aren’t routed here."),
        _ => None,
    };
    let action = if info.is_custom {
        "Remove"
    } else if info.enabled {
        "Disable"
    } else {
        "Enable"
    };
    let mut body = div().p(px(20.)).flex().flex_col().gap(px(15.)).child(
        div()
            .truncate()
            .text_size(px(13.))
            .line_height(px(24.))
            .text_color(white_a(0.5))
            .child(crate::settings::host(&info.url)),
    );
    if let Some(text) = notice {
        let tone = if info.enabled {
            info.status.tone
        } else {
            "disabled"
        };
        body = body.child(
            div()
                .flex()
                .items_center()
                .gap(px(10.))
                .p(px(12.))
                .rounded(px(10.))
                .bg(white_a(0.04))
                .child(pill(if tone == "connected" {
                    "connected"
                } else {
                    tone
                }))
                .child(
                    div()
                        .flex_1()
                        .text_size(px(13.))
                        .line_height(px(18.))
                        .text_color(white_a(0.75))
                        .child(text),
                ),
        );
    }
    body = body.child(
        div()
            .mt(px(10.))
            .flex()
            .gap(px(10.))
            .child(
                button("server-action", 44.)
                    .pt(px(2.))
                    .border_1()
                    .border_color(c(DANGER))
                    .hover(|d| d.opacity(0.75))
                    .on_click(click(on, || Act::ServerAction))
                    .child(action),
            )
            .child(
                button("server-done", 44.)
                    .border_1()
                    .border_color(white_a(0.2))
                    .hover(|d| d.opacity(0.75))
                    .on_click(click(on, || Act::Close))
                    .child("Close"),
            ),
    );
    card("server-info", body, on)
}

pub fn view(d: &NetDialog, on: &OnAct, cx: &App) -> AnyElement {
    match d {
        NetDialog::AddRelay { input, mode } => add_relay(input, mode, on, cx),
        NetDialog::Relay {
            row,
            metrics,
            logs,
            mode_open,
            copied,
        } => relay(row, metrics, logs, *mode_open, *copied, on),
        NetDialog::Server { info } => server(info, on),
    }
}
