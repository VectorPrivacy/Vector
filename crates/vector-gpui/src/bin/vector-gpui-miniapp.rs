//! Linux Mini App windows: WebKitGTK webviews in a process of their own, so the app never
//! loads WebKit and a crashed page takes only its windows down. Driven over stdin/stdout by
//! `miniapp/host_proc.rs`; it exits when the app closes the pipe.

#[cfg(not(target_os = "linux"))]
fn main() {}

#[cfg(target_os = "linux")]
#[path = "../miniapp/wire.rs"]
mod wire;

#[cfg(target_os = "linux")]
fn main() {
    host::run();
}

#[cfg(target_os = "linux")]
mod host {
    use std::cell::{Cell, RefCell};
    use std::collections::HashMap;
    use std::rc::Rc;
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicU64, Ordering};

    use gtk::glib;
    use gtk::prelude::*;
    use wry::http::{Request, Response};
    use wry::{RequestAsyncResponder, WebContext, WebViewBuilder, WebViewBuilderExtUnix, WebViewExtUnix};

    use crate::wire::{self, FromHost, Open, ToHost};

    const PLAYER: &str = "player";
    const CRASHED: &str = "Mini App Crashed Unexpectedly";

    struct Window {
        window: gtk::Window,
        view: wry::WebView,
        /// An isolated app's data store; it must outlive the view.
        _context: Option<WebContext>,
    }

    thread_local! {
        static OPEN: RefCell<HashMap<String, Window>> = RefCell::new(HashMap::new());
    }

    static OUT: Mutex<Option<std::io::Stdout>> = Mutex::new(None);
    static PENDING: Mutex<Option<HashMap<u64, RequestAsyncResponder>>> = Mutex::new(None);
    static NEXT: AtomicU64 = AtomicU64::new(1);

    fn send(msg: &FromHost, body: &[u8]) {
        if let Ok(mut out) = OUT.lock()
            && let Some(out) = out.as_mut()
            && wire::write(&mut out.lock(), msg, body).is_err()
        {
            std::process::exit(0);
        }
    }

    fn warn(what: &str) {
        eprintln!("[MiniApp host] {what}");
    }

    /// Before GTK or any thread starts, as the desktop client does.
    fn webkit_env() {
        let user_chose = std::env::var_os("WEBKIT_DISABLE_DMABUF_RENDERER").is_some()
            || std::env::var_os("WEBKIT_DMABUF_RENDERER_FORCE_SHM").is_some();
        // SAFETY: single-threaded here; the getters read the linked WebKitGTK's version.
        unsafe {
            if !user_chose {
                // DMA-BUF hand-off garbles on some GPUs; shared memory keeps acceleration on 2.54+.
                let webkit = (webkit2gtk::ffi::webkit_get_major_version(), webkit2gtk::ffi::webkit_get_minor_version());
                if webkit >= (2, 54) {
                    std::env::set_var("WEBKIT_DMABUF_RENDERER_FORCE_SHM", "1");
                } else {
                    std::env::set_var("WEBKIT_DISABLE_DMABUF_RENDERER", "1");
                }
            }
            // WebKitGTK keeps SharedArrayBuffer off even in a cross-origin isolated page unless JSC enables it.
            std::env::set_var("JSC_useSharedArrayBuffer", "1");
        }
    }

    pub fn run() {
        webkit_env();
        if let Err(e) = gtk::init() {
            warn(&format!("GTK unavailable: {e}"));
            std::process::exit(2);
        }
        *OUT.lock().unwrap() = Some(std::io::stdout());
        *PENDING.lock().unwrap() = Some(HashMap::new());
        let ctx = glib::MainContext::default();
        std::thread::Builder::new()
            .name("miniapp-pipe".into())
            .spawn(move || {
                let mut input = std::io::stdin().lock();
                while let Ok((msg, body)) = wire::read::<ToHost>(&mut input) {
                    if let ToHost::Reply { id, status, headers } = msg {
                        reply(id, status, headers, body);
                        continue;
                    }
                    ctx.invoke(move || handle(msg));
                }
                std::process::exit(0);
            })
            .expect("pipe thread");
        gtk::main();
    }

    fn reply(id: u64, status: u16, headers: Vec<(String, String)>, body: Vec<u8>) {
        let Some(responder) = PENDING.lock().ok().and_then(|mut p| p.as_mut()?.remove(&id)) else { return };
        let mut r = Response::builder().status(status);
        for (k, v) in headers {
            r = r.header(k, v);
        }
        match r.body(body) {
            Ok(r) => responder.respond(r),
            Err(_) => responder.respond(Response::builder().status(500).body(Vec::new()).unwrap()),
        }
    }

    fn handle(msg: ToHost) {
        match msg {
            ToHost::Open(spec) => {
                let label = spec.label.clone();
                if let Err(error) = build(spec) {
                    warn(&format!("window failed: {error}"));
                    send(&FromHost::Failed { label, error }, &[]);
                }
            }
            ToHost::Player { title, url, size, init } => player(title, url, size, init),
            ToHost::Focus { label, navigate } => with(&label, |o| {
                if let Some(url) = navigate {
                    let _ = o.view.load_url(&url);
                }
                o.window.show();
                o.window.present();
            }),
            ToHost::Eval { label, js } => with(&label, |o| {
                let _ = o.view.evaluate_script(&js);
            }),
            ToHost::Destroy { label } => with(&label, |o| unsafe { o.window.destroy() }),
            ToHost::Close { label } => with(&label, |o| o.window.close()),
            ToHost::Reply { .. } => {}
        }
    }

    fn with(label: &str, f: impl FnOnce(&Window)) {
        OPEN.with(|o| {
            if let Some(open) = o.borrow().get(label) {
                f(open);
            }
        })
    }

    fn serve(label: &str, request: Request<Vec<u8>>, responder: RequestAsyncResponder) {
        let id = NEXT.fetch_add(1, Ordering::Relaxed);
        if let Ok(mut p) = PENDING.lock()
            && let Some(p) = p.as_mut()
        {
            p.insert(id, responder);
        }
        let (parts, body) = request.into_parts();
        let headers = parts
            .headers
            .iter()
            .filter_map(|(k, v)| Some((k.to_string(), v.to_str().ok()?.to_string())))
            .collect();
        let msg = FromHost::Serve {
            id,
            label: label.to_string(),
            method: parts.method.to_string(),
            uri: parts.uri.to_string(),
            headers,
        };
        send(&msg, &body);
    }

    fn build(spec: Open) -> Result<(), String> {
        let window = gtk::Window::new(gtk::WindowType::Toplevel);
        window.set_title(&spec.title);
        window.set_default_size(480, 640);
        window.set_size_request(320, 480);
        let body = gtk::Box::new(gtk::Orientation::Vertical, 0);
        window.add(&body);

        let (own_scheme, own_host) = origin(&spec.url);
        let ipc_label = spec.label.clone();
        let scheme_label = spec.label.clone();
        let mut context = spec.isolated.as_ref().map(|i| WebContext::new(Some(i.data_dir.clone())));
        let mut builder = match context.as_mut() {
            Some(c) => WebViewBuilder::new_with_web_context(c),
            None => WebViewBuilder::new(),
        };
        let blank_first = spec.isolated.is_some();
        builder = builder
            .with_initialization_script_for_main_only(spec.init.clone(), false)
            .with_asynchronous_custom_protocol("webxdc".into(), move |_, request, responder| {
                serve(&scheme_label, request, responder);
            })
            .with_ipc_handler(move |request| {
                send(&FromHost::Ipc { label: ipc_label.clone(), body: request.into_body() }, &[]);
            })
            .with_navigation_handler(move |url| {
                let ok = origin(&url) == (own_scheme.clone(), own_host.clone()) || (blank_first && url == "about:blank");
                if !ok {
                    warn(&format!("blocked navigation to {url}"));
                }
                ok
            })
            .with_devtools(cfg!(debug_assertions))
            .with_url(if blank_first { "about:blank" } else { &spec.url });
        if let Some((host, port)) = spec.proxy.as_ref().filter(|_| !blank_first) {
            builder = builder.with_proxy_config(wry::ProxyConfig::Socks5(wry::ProxyEndpoint {
                host: host.clone(),
                port: port.to_string(),
            }));
        }
        let view = builder.build_gtk(&body).map_err(|e| e.to_string())?;
        // Blank first: the null proxy, with this origin exempt, must be in place before the app runs.
        if let Some(i) = &spec.isolated {
            use webkit2gtk::{NetworkProxyMode, NetworkProxySettings, WebContextExt, WebViewExt, WebsiteDataManagerExt};
            let inner = view.webview();
            vector_media::miniapps::isolated::enable_web_storage(&inner);
            let Some(manager) = inner.context().and_then(|c| c.website_data_manager()) else {
                unsafe { window.destroy() };
                return Err("could not isolate the Mini App's network".into());
            };
            let hosts: Vec<&str> = i.bypass.iter().map(String::as_str).collect();
            let mut settings = NetworkProxySettings::new(Some(i.sink.as_str()), &hosts);
            manager.set_network_proxy_settings(NetworkProxyMode::Custom, Some(&mut settings));
            view.load_url(&i.boot_url).map_err(|e| e.to_string())?;
        }

        // A crashed page would leave a blank window: it goes, and the app says why.
        {
            use webkit2gtk::WebViewExt;
            let label = spec.label.clone();
            let w = window.clone();
            view.webview().connect_web_process_terminated(move |_, _| {
                send(&FromHost::Failed { label: label.clone(), error: CRASHED.into() }, &[]);
                unsafe { w.destroy() };
            });
        }

        // A first close leaves the page instead, so unload handlers can send a last update.
        let leaving = Rc::new(Cell::new(false));
        let label = spec.label.clone();
        let farewell = spec.farewell.clone();
        window.connect_delete_event(move |w, _| {
            if leaving.replace(true) {
                return glib::Propagation::Proceed;
            }
            with(&label, |open| {
                let _ = open.view.load_url(&farewell);
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
            send(&FromHost::Closed { label: label.clone() }, &[]);
        });
        window.show_all();
        window.present();
        OPEN.with(|o| o.borrow_mut().insert(spec.label, Window { window, view, _context: context }));
        Ok(())
    }

    fn player(title: String, url: String, size: (f32, f32), init: String) {
        let reused = OPEN.with(|o| match o.borrow().get(PLAYER) {
            Some(open) => {
                let _ = open.view.load_url(&url);
                open.window.set_title(&title);
                open.window.resize(size.0 as i32, size.1 as i32);
                open.window.present();
                true
            }
            None => false,
        });
        if reused {
            return;
        }
        if let Err(error) = build_player(&title, &url, size, init) {
            warn(&format!("player window failed: {error}"));
            send(&FromHost::Failed { label: PLAYER.into(), error }, &[]);
        }
    }

    /// Its own ephemeral context: no Mini App's null proxy, and nothing kept between plays.
    fn build_player(title: &str, url: &str, size: (f32, f32), init: String) -> Result<(), String> {
        let window = gtk::Window::new(gtk::WindowType::Toplevel);
        window.set_title(title);
        window.set_default_size(size.0 as i32, size.1 as i32);
        let body = gtk::Box::new(gtk::Orientation::Vertical, 0);
        window.add(&body);
        let own = origin(url);
        let view = WebViewBuilder::new()
            .with_incognito(true)
            .with_initialization_script_for_main_only(init, true)
            .with_ipc_handler(|request| send(&FromHost::Player { body: request.into_body() }, &[]))
            .with_navigation_handler(move |url| origin(&url) == own)
            .with_new_window_req_handler(|url, _| {
                send(&FromHost::Browse { url }, &[]);
                wry::NewWindowResponse::Deny
            })
            .with_url(url)
            .build_gtk(&body)
            .map_err(|e| e.to_string())?;
        window.connect_destroy(|_| {
            OPEN.with(|o| o.borrow_mut().remove(PLAYER));
            send(&FromHost::Closed { label: PLAYER.into() }, &[]);
        });
        window.show_all();
        window.present();
        OPEN.with(|o| o.borrow_mut().insert(PLAYER.into(), Window { window, view, _context: None }));
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
}
