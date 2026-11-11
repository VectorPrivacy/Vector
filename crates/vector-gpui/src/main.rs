mod assets;
use vector_media::{audio_devices, calls, mic_access};
mod attachments;
mod backend;
mod badgecard;
mod call;
mod community;
mod createchannel;
mod createcommunity;
mod composer;
#[allow(dead_code, clippy::all)]
mod crop;
mod cropper;
mod data;
mod emoji;
mod filepreview;
mod highlight;
mod inline;
mod invite;
mod kit;
mod linkcard;
mod login;
mod markdown;
mod media;
mod newchat;
#[allow(dead_code, clippy::all)]
mod message;
mod picker;
mod preview;
mod profile;
mod qr;
#[allow(dead_code, unused_imports, unsafe_op_in_unsafe_fn, clippy::all)]
mod shared;
#[allow(dead_code, unused_imports, unsafe_op_in_unsafe_fn, clippy::all)]
mod simd;
mod startmenu;
mod netdialog;
mod security;
mod settings;
mod switcher;
mod viktor;
mod theme;
mod timeline;
mod transfer;
mod ui;
#[allow(dead_code)]
mod util;
mod viewer;

use std::path::PathBuf;


use gpui::{App, Bounds, WindowBounds, WindowDecorations, WindowOptions, prelude::*, px, size};

/// `vector-gpui [--data-dir <dir>] [--fixtures [<file>]] [--open <npub>] [--size <w>x<h>]`
///
/// A live account by default; `--fixtures` renders the parity fixtures offline instead.
struct Args {
    fixtures: Option<PathBuf>,
    data_dir: Option<PathBuf>,
    open: Option<String>,
    size: (f32, f32),
}

const FIXTURES: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/reference/fixtures.json");

fn args() -> Args {
    let mut a = Args { fixtures: None, data_dir: None, open: None, size: (1200., 760.) };
    let mut it = std::env::args().skip(1).peekable();
    while let Some(flag) = it.next() {
        match flag.as_str() {
            "--fixtures" => {
                let file = it.next_if(|v| !v.starts_with("--")).map(PathBuf::from);
                a.fixtures = Some(file.unwrap_or_else(|| PathBuf::from(FIXTURES)));
            }
            "--data-dir" => a.data_dir = it.next().map(PathBuf::from),
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
    let fixtures = args.fixtures.as_ref().map(|f| {
        let store = data::Store::from_fixtures(f).expect("fixtures");
        let open = args.open.clone().or_else(|| std::env::var("VECTOR_OPEN_FIRST").ok().and(store.chats.first().map(|c| c.id.clone())));
        (store, open)
    });
    let events = fixtures.is_none().then(|| backend::start(args.data_dir.clone().unwrap_or_else(backend::default_data_dir)));

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
            |window, cx| {
                cx.new(|cx| match (fixtures, events) {
                    (Some((store, open)), _) => ui::VectorApp::fixtures(store, open, window, cx),
                    (None, Some(events)) => ui::VectorApp::live(events, window, cx),
                    (None, None) => unreachable!(),
                })
            },
        )
        .expect("main window");
        cx.activate(true);
    });
}
