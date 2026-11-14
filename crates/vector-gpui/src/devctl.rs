//! Dev builds only (`devctl` feature): drive and measure a running window from a shell.
//! A second launch with `dev:<command>` arguments hands them over like any link; each command
//! writes its answer to `$TMPDIR/vector-devctl.json`.
//!
//! - `dev:state`: screen, what's open, the Communities and their channels, and frame stats since
//!   the last read (whole-window draws and their time, presents, `VectorApp::render` calls)
//! - `dev:community:<id>` / `dev:channel:<id>` / `dev:chat:<id>`: open it
//! - `dev:scroll:<px>`: scroll the open timeline by that much (positive is down)
//! - `dev:bottom`: jump the open timeline to its newest message
//! - `dev:pin:<digits>`: type a PIN into the lock screen
//! - `dev:typing:<chat>:<npub>`: show that npub typing in that chat, as if a notice arrived
//! - `dev:move:<x>,<y>` / `dev:click:<x>,<y>` / `dev:rclick:<x>,<y>`: the mouse, in window points
//! - `dev:wheel:<x>,<y>,<dy>`: scroll what is under the point (positive is down)
//! - `dev:type:<text>`: type into whatever has focus
//! - `dev:key:<keystroke>`: press one, as GPUI spells them (`enter`, `escape`, `cmd-k`)
//! - `dev:shot`: the window as `$TMPDIR/vector-devctl.png` (macOS and Windows)

use std::sync::atomic::{AtomicU64, Ordering};

use gpui::{
    Context, Keystroke, Modifiers, MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent,
    PlatformInput, ScrollDelta, ScrollEvent, Window, point, px,
};
use serde_json::json;

use crate::ui::VectorApp;

static RENDERS: AtomicU64 = AtomicU64::new(0);
static RENDER_NANOS: AtomicU64 = AtomicU64::new(0);

/// Times one `VectorApp::render`.
pub struct Timed(std::time::Instant);

impl Timed {
    pub fn start() -> Self {
        Self(std::time::Instant::now())
    }
}

impl Drop for Timed {
    fn drop(&mut self) {
        RENDERS.fetch_add(1, Ordering::Relaxed);
        RENDER_NANOS.fetch_add(self.0.elapsed().as_nanos() as u64, Ordering::Relaxed);
    }
}

fn answer(v: serde_json::Value) {
    let path = std::env::temp_dir().join("vector-devctl.json");
    let _ = std::fs::write(path, serde_json::to_vec_pretty(&v).unwrap_or_default());
}

pub fn is_command(arg: &str) -> bool {
    arg.starts_with("dev:")
}

pub fn run(app: &mut VectorApp, cmd: &str, window: &mut Window, cx: &mut Context<VectorApp>) {
    let cmd = cmd.trim_start_matches("dev:");
    let (verb, rest) = cmd.split_once(':').unwrap_or((cmd, ""));
    match verb {
        "state" => {
            let (draws, draw_ns, presents) = gpui::draw_stats::take();
            let renders = RENDERS.swap(0, Ordering::Relaxed);
            let render_ns = RENDER_NANOS.swap(0, Ordering::Relaxed);
            let mut v = app.dev_state();
            v["frames"] = json!({
                "draws": draws,
                "presents": presents,
                "draw_ms": draw_ns as f64 / 1e6,
                "renders": renders,
                "render_ms": render_ns as f64 / 1e6,
            });
            v["active"] = json!(window.is_window_active());
            answer(v);
        }
        "community" => app.dev_open_community(rest.to_string(), window, cx),
        "channel" => app.dev_open_channel(rest.to_string(), window, cx),
        "chat" => app.dev_open_chat(rest.to_string().into(), window, cx),
        "scroll" => {
            let dy = rest.parse::<f32>().unwrap_or(0.);
            app.dev_timeline().scroll_by(px(dy));
            cx.notify();
        }
        "bottom" => {
            let t = app.dev_timeline();
            let n = t.item_count();
            if n > 0 {
                t.scroll_to_reveal_item(n - 1);
            }
            cx.notify();
        }
        "pin" => app.dev_pin(rest, window, cx),
        "typing" => {
            if let Some((chat, who)) = rest.rsplit_once(':') {
                app.dev_typing(chat, who, cx);
            }
        }
        "move" | "click" | "rclick" | "wheel" => {
            let n: Vec<f32> = rest.split(',').filter_map(|v| v.trim().parse().ok()).collect();
            let Some((&x, &y)) = n.first().zip(n.get(1)) else {
                return answer(json!({ "error": "expected x,y" }));
            };
            let at = point(px(x), px(y));
            let mut events = vec![PlatformInput::MouseMove(MouseMoveEvent { position: at, pressed_button: None, modifiers: Modifiers::default() })];
            let button = if verb == "rclick" { MouseButton::Right } else { MouseButton::Left };
            match verb {
                "click" | "rclick" => {
                    events.push(PlatformInput::MouseDown(MouseDownEvent { button, position: at, click_count: 1, ..Default::default() }));
                    events.push(PlatformInput::MouseUp(MouseUpEvent { button, position: at, click_count: 1, ..Default::default() }));
                }
                "wheel" => events.push(PlatformInput::Scroll(ScrollEvent {
                    position: at,
                    delta: ScrollDelta::Pixels(point(px(0.), px(-n.get(2).copied().unwrap_or(0.)))),
                    ..Default::default()
                })),
                _ => {}
            }
            later(window, cx, move |window, cx| {
                for e in events {
                    window.dispatch_event(e, cx);
                }
            });
        }
        "type" => {
            let text = rest.to_string();
            later(window, cx, move |window, cx| {
                for ch in text.chars() {
                    let key = if ch == ' ' { "space".to_string() } else { ch.to_string() };
                    window.dispatch_keystroke(
                        Keystroke { key, key_char: Some(ch.to_string()), modifiers: Modifiers::default(), ..Default::default() },
                        cx,
                    );
                }
            });
        }
        // parse wants a static source: a dev command is small and rare, so leak it.
        "key" => match Keystroke::parse(Box::leak(rest.to_string().into_boxed_str())).map_err(|e| e.to_string()) {
            Ok(k) => later(window, cx, move |window, cx| {
                window.dispatch_keystroke(k, cx);
            }),
            Err(e) => answer(json!({ "error": e })),
        },
        "shot" => answer(shot()),
        _ => answer(json!({ "error": format!("unknown command {verb}") })),
    }
}

/// Input dispatched once the app's own update has ended, as the platform's would be.
fn later(window: &mut Window, cx: &mut Context<VectorApp>, f: impl FnOnce(&mut Window, &mut gpui::App) + 'static) {
    cx.spawn_in(window, async move |_, cx| {
        cx.update(|window, cx| f(window, cx)).ok();
    })
    .detach();
}

#[cfg(not(target_os = "linux"))]
fn shot() -> serde_json::Value {
    let me = std::process::id();
    let path = std::env::temp_dir().join("vector-devctl.png");
    let found = xcap::Window::all()
        .map_err(|e| e.to_string())
        .and_then(|ws| {
            ws.into_iter()
                .filter(|w| w.pid().ok() == Some(me) && !w.is_minimized().unwrap_or(false))
                .max_by_key(|w| w.width().unwrap_or(0) * w.height().unwrap_or(0))
                .ok_or_else(|| "no window of this process".to_string())
        })
        .and_then(|w| w.capture_image().map_err(|e| e.to_string()))
        .and_then(|img| img.save(&path).map(|_| (img.width(), img.height())).map_err(|e| e.to_string()));
    match found {
        Ok((w, h)) => json!({ "shot": path, "width": w, "height": h }),
        Err(e) => json!({ "error": e }),
    }
}

#[cfg(target_os = "linux")]
fn shot() -> serde_json::Value {
    json!({ "error": "on Linux, capture with the X server (import -window)" })
}
