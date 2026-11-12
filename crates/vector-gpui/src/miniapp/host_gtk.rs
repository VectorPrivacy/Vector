//! Mini App windows on Linux: WebKitGTK webviews (through wry) on a GTK thread of their own,
//! beside GPUI's event loop. Everything GTK lives on that thread; callers post to it.

use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::OnceLock;

use gtk::glib;
use gtk::prelude::*;
use wry::{WebViewBuilder, WebViewBuilderExtUnix};

use super::{Event, Spec};

struct Open {
    window: gtk::Window,
    view: wry::WebView,
}

thread_local! {
    static OPEN: RefCell<HashMap<String, Open>> = RefCell::new(HashMap::new());
}

static CONTEXT: OnceLock<Option<glib::MainContext>> = OnceLock::new();
static EVENTS: OnceLock<Box<dyn Fn(Event) + Send + Sync>> = OnceLock::new();

pub fn start(events: impl Fn(Event) + Send + Sync + 'static) {
    let _ = EVENTS.set(Box::new(events));
}

fn emit(e: Event) {
    if let Some(f) = EVENTS.get() {
        f(e);
    }
}

/// The GTK thread's context, started on first use.
fn context() -> Result<&'static glib::MainContext, String> {
    CONTEXT
        .get_or_init(|| {
            let (tx, rx) = std::sync::mpsc::sync_channel(1);
            let spawned = std::thread::Builder::new()
                .name("miniapp-gtk".into())
                .spawn(move || {
                    if let Err(e) = gtk::init() {
                        vector_core::log_warn!("[MiniApp] GTK unavailable: {e}");
                        let _ = tx.send(None);
                        return;
                    }
                    let _ = tx.send(Some(glib::MainContext::default()));
                    gtk::main();
                });
            spawned.ok().and_then(|_| rx.recv().ok().flatten())
        })
        .as_ref()
        .ok_or_else(|| "Mini Apps need GTK and WebKitGTK on this system".to_string())
}

fn on_gtk(f: impl FnOnce() + Send + 'static) -> Result<(), String> {
    context()?.invoke(f);
    Ok(())
}

pub fn open(spec: Spec) -> Result<(), String> {
    on_gtk(move || {
        let label = spec.label.clone();
        if let Err(e) = build(spec) {
            vector_core::log_warn!("[MiniApp] window failed: {e}");
            emit(Event::Failed(label, e));
        }
    })
}

fn build(spec: Spec) -> Result<(), String> {
    let window = gtk::Window::new(gtk::WindowType::Toplevel);
    window.set_title(&spec.title);
    window.set_default_size(480, 640);
    window.set_size_request(320, 480);
    let body = gtk::Box::new(gtk::Orientation::Vertical, 0);
    window.add(&body);

    let (own_scheme, own_host) = origin(&spec.url);
    let ipc_label = spec.label.clone();
    let scheme_label = spec.label.clone();
    let mut builder = WebViewBuilder::new()
        .with_initialization_script_for_main_only(spec.init.clone(), false)
        .with_asynchronous_custom_protocol("webxdc".into(), move |_, request, responder| {
            vector_media::miniapps::serve(&scheme_label, request, move |response| {
                responder.respond(response)
            });
        })
        .with_ipc_handler(move |request| {
            emit(Event::Ipc(ipc_label.clone(), request.into_body()));
        })
        .with_navigation_handler(move |url| {
            let ok = origin(&url) == (own_scheme.clone(), own_host.clone());
            if !ok {
                vector_core::log_warn!("[MiniApp] blocked navigation to {url}");
            }
            ok
        })
        .with_devtools(cfg!(debug_assertions))
        .with_url(&spec.url);
    if let Some((host, port)) = &spec.proxy {
        builder = builder.with_proxy_config(wry::ProxyConfig::Socks5(wry::ProxyEndpoint {
            host: host.clone(),
            port: port.to_string(),
        }));
    }
    let view = builder.build_gtk(&body).map_err(|e| e.to_string())?;

    // A first close leaves the page instead, so unload handlers can send a last update.
    let leaving = std::rc::Rc::new(std::cell::Cell::new(false));
    let label = spec.label.clone();
    let farewell = spec.farewell.clone();
    window.connect_delete_event(move |w, _| {
        if leaving.replace(true) {
            return glib::Propagation::Proceed;
        }
        OPEN.with(|o| {
            if let Some(open) = o.borrow().get(&label) {
                let _ = open.view.load_url(&farewell);
            }
        });
        w.hide();
        let w = w.clone();
        glib::timeout_add_local_once(std::time::Duration::from_millis(300), move || {
            w.close();
        });
        glib::Propagation::Stop
    });
    let label = spec.label.clone();
    window.connect_destroy(move |_| {
        OPEN.with(|o| o.borrow_mut().remove(&label));
        emit(Event::Closed(label.clone()));
    });
    window.show_all();
    window.present();
    OPEN.with(|o| o.borrow_mut().insert(spec.label, Open { window, view }));
    Ok(())
}

fn origin(url: &str) -> (String, String) {
    match url::Url::parse(url) {
        Ok(u) => (
            u.scheme().to_string(),
            format!("{}:{:?}", u.host_str().unwrap_or(""), u.port_or_known_default()),
        ),
        Err(_) => (String::new(), String::new()),
    }
}

fn with(label: String, f: impl FnOnce(&Open) + Send + 'static) {
    let _ = on_gtk(move || {
        OPEN.with(|o| {
            if let Some(open) = o.borrow().get(&label) {
                f(open);
            }
        })
    });
}

pub fn focus(label: &str, navigate: Option<String>) {
    with(label.to_string(), move |o| {
        if let Some(url) = navigate {
            let _ = o.view.load_url(&url);
        }
        o.window.show();
        o.window.present();
    });
}

pub fn eval(label: &str, js: String) {
    with(label.to_string(), move |o| {
        let _ = o.view.evaluate_script(&js);
    });
}

/// Gone at once, with no farewell page: an account swap.
pub fn destroy(label: &str) {
    with(label.to_string(), |o| unsafe { o.window.destroy() });
}

pub fn close(label: &str) {
    with(label.to_string(), |o| o.window.close());
}
