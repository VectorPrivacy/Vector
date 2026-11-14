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

use std::sync::atomic::{AtomicU64, Ordering};

use gpui::{Context, Window, px};
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
        _ => answer(json!({ "error": format!("unknown command {verb}") })),
    }
}
