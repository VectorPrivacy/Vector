//! The main window's place between launches: size, position, display, maximized or fullscreen.
//! Kept in our own file in GPUI's coordinates, and mirrored into Tauri's window-state file so a
//! switch between the two clients keeps the window where it was.

use std::path::PathBuf;

use gpui::{App, Bounds, DisplayId, Pixels, Size, Window, WindowBounds, point, px, size};
use serde::{Deserialize, Serialize};

static ENABLED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Saving is on only for a real launch: fixtures and an explicit `--size` leave the user's alone.
pub fn enable() {
    ENABLED.store(true, std::sync::atomic::Ordering::Relaxed);
}

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq)]
#[serde(rename_all = "lowercase")]
enum Mode {
    Windowed,
    Maximized,
    Fullscreen,
}

#[derive(Serialize, Deserialize, PartialEq)]
struct Saved {
    display: Option<String>,
    mode: Mode,
    x: f32,
    y: f32,
    w: f32,
    h: f32,
}

fn ours() -> Option<PathBuf> {
    Some(dirs::config_dir()?.join("io.vectorapp").join("gpui-window.json"))
}

/// tauri-plugin-window-state's file, in the app's config dir.
fn tauris() -> Option<PathBuf> {
    Some(dirs::config_dir()?.join("io.vectorapp").join(".window-state.json"))
}

fn wrap(mode: Mode, b: Bounds<Pixels>) -> WindowBounds {
    match mode {
        Mode::Windowed => WindowBounds::Windowed(b),
        Mode::Maximized => WindowBounds::Maximized(b),
        Mode::Fullscreen => WindowBounds::Fullscreen(b),
    }
}

/// Where to open: the saved frame on the display it was saved on, held inside that display; else
/// Tauri's saved size, or `fresh`, centred on the primary display.
pub fn restore(fresh: Size<Pixels>, min: Size<Pixels>, cx: &App) -> (WindowBounds, Option<DisplayId>) {
    let saved: Option<Saved> = ours()
        .and_then(|p| std::fs::read(p).ok())
        .and_then(|b| serde_json::from_slice(&b).ok());
    if let Some(s) = saved {
        let display = s.display.as_deref().and_then(|uuid| {
            cx.displays().into_iter().find(|d| d.uuid().ok().is_some_and(|u| u.to_string() == uuid))
        });
        if let Some(d) = display {
            let area = d.bounds().size;
            let w = px(s.w).max(min.width).min(area.width);
            let h = px(s.h).max(min.height).min(area.height);
            let x = px(s.x).max(px(0.)).min(area.width - w);
            let y = px(s.y).max(px(0.)).min(area.height - h);
            return (wrap(s.mode, Bounds::new(point(x, y), size(w, h))), Some(d.id()));
        }
        let b = Bounds::centered(None, size(px(s.w).max(min.width), px(s.h).max(min.height)), cx);
        return (wrap(s.mode, b), None);
    }
    let (mode, sz) = from_tauri().unwrap_or((Mode::Windowed, fresh));
    let area = cx.primary_display().map(|d| d.bounds().size);
    let sz = area.map_or(sz, |a| size(sz.width.max(min.width).min(a.width), sz.height.max(min.height).min(a.height)));
    (wrap(mode, Bounds::centered(None, sz, cx)), None)
}

/// Tauri's saved size and state. Its pixels are physical; the scale they were taken at isn't
/// kept, so assume a Retina Mac's 2 and 1 elsewhere.
fn from_tauri() -> Option<(Mode, Size<Pixels>)> {
    let v: serde_json::Value = serde_json::from_slice(&std::fs::read(tauris()?).ok()?).ok()?;
    let main = v.get("main")?;
    let int = |k: &str| main.get(k).and_then(|v| v.as_f64());
    let flag = |k: &str| main.get(k).and_then(|v| v.as_bool()).unwrap_or(false);
    let scale = if cfg!(target_os = "macos") { 2. } else { 1. };
    let mode = if flag("fullscreen") {
        Mode::Fullscreen
    } else if flag("maximized") {
        Mode::Maximized
    } else {
        Mode::Windowed
    };
    Some((mode, size(px((int("width")? / scale) as f32), px((int("height")? / scale) as f32))))
}

/// Remember the window as it is now. Returns false when nothing changed.
pub fn save(window: &Window, cx: &App) -> bool {
    if !ENABLED.load(std::sync::atomic::Ordering::Relaxed) {
        return false;
    }
    let (mode, b) = match window.window_bounds() {
        WindowBounds::Windowed(b) => (Mode::Windowed, b),
        WindowBounds::Maximized(b) => (Mode::Maximized, b),
        WindowBounds::Fullscreen(b) => (Mode::Fullscreen, b),
    };
    let display = window.display(cx);
    let s = Saved {
        display: display.as_ref().and_then(|d| d.uuid().ok()).map(|u| u.to_string()),
        mode,
        x: f32::from(b.origin.x),
        y: f32::from(b.origin.y),
        w: f32::from(b.size.width),
        h: f32::from(b.size.height),
    };
    let Some(path) = ours() else { return false };
    let Ok(bytes) = serde_json::to_vec(&s) else { return false };
    if std::fs::read(&path).ok().as_deref() == Some(bytes.as_slice()) {
        return false;
    }
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let _ = std::fs::write(&path, bytes);
    let origin = display.map_or(point(px(0.), px(0.)), |d| d.bounds().origin);
    to_tauri(&s, (f32::from(origin.x), f32::from(origin.y)), window.scale_factor());
    true
}

/// Mirror into Tauri's file in its physical pixels, keeping whatever else it holds.
fn to_tauri(s: &Saved, origin: (f32, f32), scale: f32) {
    let Some(path) = tauris() else { return };
    let mut v: serde_json::Value = std::fs::read(&path)
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .filter(serde_json::Value::is_object)
        .unwrap_or_else(|| serde_json::json!({}));
    let main = v
        .as_object_mut()
        .map(|o| o.entry("main").or_insert_with(|| serde_json::json!({})));
    let Some(serde_json::Value::Object(main)) = main else { return };
    let phys = |n: f32| serde_json::json!((n * scale).round() as i64);
    main.insert("width".into(), phys(s.w));
    main.insert("height".into(), phys(s.h));
    main.insert("x".into(), phys(origin.0 + s.x));
    main.insert("y".into(), phys(origin.1 + s.y));
    main.insert("maximized".into(), serde_json::json!(s.mode == Mode::Maximized));
    main.insert("fullscreen".into(), serde_json::json!(s.mode == Mode::Fullscreen));
    if let Ok(bytes) = serde_json::to_vec(&v) {
        let _ = std::fs::write(&path, bytes);
    }
}
