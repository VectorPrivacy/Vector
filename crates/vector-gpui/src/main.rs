mod assets;
mod composer;
mod data;
mod theme;
mod ui;

use std::path::PathBuf;

use gpui::{App, Bounds, WindowBounds, WindowDecorations, WindowOptions, prelude::*, px, size};

/// `vector-gpui [--fixtures <file>] [--open <npub>] [--size <w>x<h>]`
struct Args {
    fixtures: PathBuf,
    open: Option<String>,
    size: (f32, f32),
}

fn args() -> Args {
    let mut a = Args {
        fixtures: PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/reference/fixtures.json")),
        open: None,
        size: (1200., 760.),
    };
    let mut it = std::env::args().skip(1);
    while let Some(flag) = it.next() {
        match flag.as_str() {
            "--fixtures" => a.fixtures = it.next().map(PathBuf::from).unwrap_or(a.fixtures),
            "--open" => a.open = it.next(),
            "--size" => {
                if let Some((w, h)) = it.next().and_then(|s| s.split_once('x').map(|(w, h)| (w.parse(), h.parse())))
                    && let (Ok(w), Ok(h)) = (w, h)
                {
                    a.size = (w, h);
                }
            }
            _ => {}
        }
    }
    a
}

fn main() {
    env_logger::init();
    let args = args();
    let store = data::Store::from_fixtures(&args.fixtures).expect("fixtures");
    let open = args.open.or_else(|| std::env::var("VECTOR_OPEN_FIRST").ok().and(store.chats.first().map(|c| c.id.clone())));

    gpui_platform::application().with_assets(assets::Assets).run(move |cx: &mut App| {
        cx.text_system().add_fonts(assets::fonts()).expect("bundled fonts");
        composer::bind_keys(cx);
        let bounds = Bounds::centered(None, size(px(args.size.0), px(args.size.1)), cx);
        cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                titlebar: None,
                window_decorations: Some(WindowDecorations::Client),
                window_min_size: Some(size(px(385.), px(575.))),
                app_id: Some("io.vectorapp.gpui".into()),
                ..Default::default()
            },
            |window, cx| cx.new(|cx| ui::VectorApp::new(store, open, window, cx)),
        )
        .expect("main window");
        cx.activate(true);
    });
}
