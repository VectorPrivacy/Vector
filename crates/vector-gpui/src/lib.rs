//! The client: `run` opens it, from `main.rs` on the desktop and `gpui_android_main` on Android.

#[macro_use]
extern crate vector_core;

mod album;
#[cfg(target_os = "android")]
mod android;
mod applog;
mod assets;
use vector_media::{audio_devices, calls, mic_access};
mod attachments;
mod audio;
mod audit;
mod jxl;
mod backend;
mod badge;
mod badgecard;
mod call;
mod chaneditor;
mod commsettings;
mod coretext;
mod community;
mod rail;
mod voice;
mod miniprofile;
mod pings;
mod pins;
mod search;
mod pickers;
mod edithistory;
mod textdiff;
#[cfg(test)]
mod atlas_border;
#[cfg(test)]
mod textfuzz;
#[cfg(test)]
mod tree_audit;
mod payaddr;
mod reactpop;
mod rekey;
mod relayinfo;
mod golinks;
mod scrollbar;
mod serverinfo;
mod secretguard;
mod cmdform;
mod commands;
mod composer;
mod createchannel;
mod createcommunity;
#[allow(dead_code, clippy::all)]
mod crop;
mod cropper;
mod data;
mod dates;
mod decorate;
mod chrome;
mod deeplink;
mod embeds;
mod emoji;
mod fileclip;
mod filepreview;
mod highlight;
#[cfg(test)]
mod i18n_audit;
mod inline;
mod invite;
mod kit;
mod linkcard;
mod login;
mod markdown;
mod marks;
mod lang;
mod langnames;
mod lyrics;
mod media;
mod spell;
mod profilewatch;
mod untrack;
mod model3d;
mod trim;
#[cfg(target_os = "linux")]
mod vkselect;
mod callvideo;
mod callpics;
mod downgrade;
mod screenpick;
mod miniapp;
mod mentions;
#[allow(dead_code, clippy::all)]
mod message;
mod moderation;
mod netdialog;
mod netglyph;
mod netinfo;
mod routes;
mod routing;
mod newchat;
mod nexus;
mod packdetails;
mod packs;
mod paste_md;
mod pic;
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
mod scan;
mod shortcodes;
#[allow(dead_code, unused_imports, unsafe_op_in_unsafe_fn, clippy::all)]
mod simd;
mod startmenu;
mod switcher;
mod textcolor;
mod t;
mod texttime;
mod theme;
mod timeline;
mod transcript;
mod wallpaper;
mod winstate;
mod transfer;
mod ui;
mod uploaderr;
#[allow(dead_code)]
mod util;
mod viewer;
mod viktor;
#[cfg(feature = "devctl")]
mod devctl;
mod pane;
mod select;
mod sprite;
mod stretch;
mod video;
mod heif;
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
    sized: bool,
}

const FIXTURES: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/reference/fixtures.json");

fn args() -> Args {
    let mut a = Args {
        fixtures: None,
        data_dir: None,
        open: None,
        size: (1200., 760.),
        sized: false,
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
                a.sized = true;
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

#[cfg(feature = "dhat-heap")]
#[global_allocator]
static ALLOC: dhat::Alloc = dhat::Alloc;

#[cfg(feature = "dhat-heap")]
pub static DHAT: std::sync::Mutex<Option<dhat::Profiler>> = std::sync::Mutex::new(None);

pub fn run() {
    trim::cap_arenas();
    #[cfg(feature = "dhat-heap")]
    {
        *DHAT.lock().unwrap_or_else(|e| e.into_inner()) = Some(dhat::Profiler::builder().trim_backtraces(Some(12)).build());
    }
    #[cfg(target_os = "linux")]
    vkselect::narrow();
    let args = args();
    // A fixture run without its own data dir keeps out of the real account's log.
    applog::init(args.data_dir.clone().or_else(|| args.fixtures.is_none().then(backend::default_data_dir)));
    log::info!("starting {}", env!("CARGO_PKG_VERSION"));
    jxl_oxide::integration::register_image_decoding_hook();
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
    // A devctl build listens in fixture mode too, so its window can be driven.
    let listens = fixtures.is_none() || cfg!(feature = "devctl");
    if listens && !deeplink::claim(&data_dir, &deeplink::from_args(), links_tx.clone()) {
        return;
    }
    if fixtures.is_none() {
        fileclip::purge();
    }
    let packs = fixtures.is_none().then(|| lang::packs_dir(&data_dir));
    let events = fixtures.is_none().then(|| backend::start(data_dir));
    lang::init(packs);
    log::info!("core started");

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
        miniapp::attach(cx);
        log::info!("launched: opening the window");
        let min = size(px(385.), px(575.));
        let remember = fixtures.is_none() && !args.sized;
        let (window_bounds, display_id) = if remember {
            winstate::restore(size(px(args.size.0), px(args.size.1)), min, cx)
        } else {
            (WindowBounds::Windowed(Bounds::centered(None, size(px(args.size.0), px(args.size.1)), cx)), None)
        };
        let window = cx
            .open_window(
                WindowOptions {
                    window_bounds: Some(window_bounds),
                    display_id,
                    titlebar: chrome::titlebar(),
                    window_decorations: Some(WindowDecorations::Client),
                    window_min_size: Some(min),
                    app_id: Some("io.vectorapp.gpui".into()),
                    ..Default::default()
                },
                |window, cx| {
                    window.set_window_title("Vector"); // i18n-exempt: the app's name
                    chrome::hide_native_controls(window);
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
        log::info!("window open");
        cx.activate(true);
        // Closing the main window ends the app: nothing else can bring it back.
        let main = window.window_id();
        cx.on_window_closed(move |cx, id| {
            if id == main {
                cx.quit();
            }
        })
        .detach();
        // Most of Tauri's hovers carry `transition: background-color .15s` and its kin.
        gpui::set_hover_ease(std::time::Duration::from_millis(150));
        cx.on_app_quit(|_| {
            vector_media::whisper::unload();
            async {}
        })
        .detach();
        if remember {
            winstate::enable();
            window
                .update(cx, |_, window, cx| {
                    let generation = std::rc::Rc::new(std::cell::Cell::new(0u64));
                    cx.observe_window_bounds(window, move |_, window, cx| {
                        let now = generation.get() + 1;
                        generation.set(now);
                        let generation = generation.clone();
                        cx.spawn_in(window, async move |_, cx| {
                            cx.background_executor().timer(std::time::Duration::from_millis(400)).await;
                            if generation.get() == now {
                                cx.update(|window, cx| winstate::save(window, cx)).ok();
                            }
                        })
                        .detach();
                    })
                    .detach();
                })
                .ok();
        }
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

/// Android's entry, which the activity starts on GPUI's own thread (vendor/gpui_android).
#[cfg(target_os = "android")]
#[unsafe(no_mangle)]
pub fn gpui_android_main() {
    // Unset, the temporary folder is one apps can't write to.
    if let Some(cache) = android::cache_dir() {
        // SAFETY: before the client starts a thread of its own.
        unsafe { std::env::set_var("TMPDIR", cache) };
        // Copies of what the picker chose: nothing reads them after the run that picked them.
        let _ = std::fs::remove_dir_all(cache.join("picked"));
        // Test builds time the GPU once per build: the log then says where frame time goes.
        #[cfg(feature = "test-build")]
        {
            let marker = cache.join(format!("gpu-self-test-{}", option_env!("GITHUB_SHA").unwrap_or("local")));
            if !marker.exists() && std::fs::write(&marker, b"").is_ok() {
                // SAFETY: as above.
                unsafe { std::env::set_var("VECTOR_GPU_SELF_TEST", "1") };
            }
        }
    }
    // An Amber account signs through the signer app from its first event.
    android::external_signer::register();
    run();
}
