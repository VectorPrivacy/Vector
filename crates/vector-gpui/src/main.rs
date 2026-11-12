mod album;
mod assets;
use vector_media::{audio_devices, calls, mic_access};
mod attachments;
mod audio;
mod audit;
mod backend;
mod badgecard;
mod call;
mod chaneditor;
mod commsettings;
mod community;
mod cmdform;
mod commands;
mod composer;
mod createchannel;
mod createcommunity;
#[allow(dead_code, clippy::all)]
mod crop;
mod cropper;
mod data;
mod decorate;
mod deeplink;
mod embeds;
mod emoji;
mod filepreview;
mod highlight;
mod inline;
mod invite;
mod kit;
mod linkcard;
mod login;
mod markdown;
mod langnames;
mod lyrics;
mod media;
mod miniapp;
mod mentions;
#[allow(dead_code, clippy::all)]
mod message;
mod moderation;
mod netdialog;
mod newchat;
mod nexus;
mod packdetails;
mod packs;
mod picker;
mod policies;
mod popout;
mod preview;
mod profile;
mod qr;
mod recorder;
mod roles;
mod security;
mod settings;
#[allow(dead_code, unused_imports, unsafe_op_in_unsafe_fn, clippy::all)]
mod shared;
mod shortcodes;
#[allow(dead_code, unused_imports, unsafe_op_in_unsafe_fn, clippy::all)]
mod simd;
mod startmenu;
mod switcher;
mod textcolor;
mod texttime;
mod theme;
mod timeline;
mod transcript;
mod wallpaper;
mod transfer;
mod ui;
#[allow(dead_code)]
mod util;
mod viewer;
mod viktor;
mod video;
#[cfg(feature = "video")]
mod yuv;

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
    let mut a = Args {
        fixtures: None,
        data_dir: None,
        open: None,
        size: (1200., 760.),
    };
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
                if let Some((w, h)) = it
                    .next()
                    .and_then(|s| s.split_once('x').map(|(w, h)| (w.parse(), h.parse())))
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
        let open = args.open.clone().or_else(|| {
            std::env::var("VECTOR_OPEN_FIRST")
                .ok()
                .and(store.chats.first().map(|c| c.id.clone()))
        });
        (store, open)
    });
    let (links_tx, mut links) = futures::channel::mpsc::unbounded::<String>();
    for url in deeplink::from_args() {
        links_tx.unbounded_send(url).ok();
    }
    let data_dir = args
        .data_dir
        .clone()
        .unwrap_or_else(backend::default_data_dir);
    if fixtures.is_none() && !deeplink::claim(&data_dir, &deeplink::from_args(), links_tx.clone()) {
        return;
    }
    let events = fixtures.is_none().then(|| backend::start(data_dir));

    let os_links = links_tx.clone();
    let app = gpui_platform::application().with_assets(assets::Assets);
    app.on_open_urls(move |urls| {
        for url in urls {
            os_links.unbounded_send(url).ok();
        }
    });
    app.run(move |cx: &mut App| {
        cx.text_system()
            .add_fonts(assets::fonts())
            .expect("bundled fonts");
        composer::bind_keys(cx);
        let bounds = Bounds::centered(None, size(px(args.size.0), px(args.size.1)), cx);
        let window = cx
            .open_window(
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
                        (Some((store, open)), _) => {
                            ui::VectorApp::fixtures(store, open, window, cx)
                        }
                        (None, Some(events)) => ui::VectorApp::live(events, window, cx),
                        (None, None) => unreachable!(),
                    })
                },
            )
            .expect("main window");
        cx.activate(true);
        let _keep = links_tx;
        cx.spawn(async move |cx| {
            use futures::StreamExt;
            while let Some(url) = links.next().await {
                let ok = window.update(cx, |app, window, cx| {
                    if url.is_empty() {
                        window.activate_window();
                    } else {
                        app.deep_link(&url, window, cx);
                    }
                });
                if ok.is_err() {
                    break;
                }
            }
        })
        .detach();
    });
}
