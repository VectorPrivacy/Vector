//! Mini App windows on macOS and Windows: a GPUI window each, the OS webview (through wry)
//! laid over its whole content. GPUI runs the platform's own loop, so the views live on its
//! thread; callers post to it.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::sync::OnceLock;
use std::time::Duration;

use futures::StreamExt;
use futures::channel::mpsc;
use gpui::{
    App, AppContext, Bounds, Context, IntoElement, Render, Styled, TitlebarOptions, Window, WindowBounds,
    WindowHandle, WindowOptions, div, px, rgb, size,
};
use wry::{WebContext, WebViewBuilder};

use super::{Event, PLAYER, PlayerSpec, Spec};

type Job = Box<dyn FnOnce(&mut App) + Send>;

static JOBS: OnceLock<mpsc::UnboundedSender<Job>> = OnceLock::new();
static EVENTS: OnceLock<Box<dyn Fn(Event) + Send + Sync>> = OnceLock::new();

thread_local! {
    static OPEN: RefCell<HashMap<String, WindowHandle<Page>>> = RefCell::new(HashMap::new());
}

pub fn start(events: impl Fn(Event) + Send + Sync + 'static) {
    let _ = EVENTS.set(Box::new(events));
}

/// Serves the host's calls on GPUI's thread from here on.
pub fn attach(cx: &mut App) {
    let (tx, mut rx) = mpsc::unbounded::<Job>();
    if JOBS.set(tx).is_err() {
        return;
    }
    cx.spawn(async move |cx| {
        while let Some(job) = rx.next().await {
            cx.update(job);
        }
    })
    .detach();
}

fn emit(e: Event) {
    if let Some(f) = EVENTS.get() {
        f(e);
    }
}

fn on_ui(f: impl FnOnce(&mut App) + Send + 'static) -> Result<(), String> {
    let jobs = JOBS.get().ok_or("The app isn't ready yet")?;
    jobs.unbounded_send(Box::new(f)).map_err(|_| "The app is closing".to_string())
}

struct Page {
    view: Option<wry::WebView>,
    /// An isolated app's data store; it must outlive the view.
    _context: Option<WebContext>,
    farewell: String,
    leaving: Cell<bool>,
    laid: Cell<(f32, f32)>,
}

impl Page {
    /// A first close leaves the page instead, so unload handlers can send a last update.
    /// False once it has already left.
    fn farewell(&self) -> bool {
        if self.leaving.replace(true) {
            return false;
        }
        if let Some(v) = &self.view {
            let _ = v.load_url(&self.farewell);
            let _ = v.set_visible(false);
        }
        true
    }

    fn leave(&self, window: &mut Window, cx: &mut App) {
        if self.farewell() {
            remove_soon(window, cx);
        } else {
            window.remove_window();
        }
    }
}

fn remove_soon(window: &mut Window, cx: &mut App) {
    window
        .spawn(cx, async move |cx| {
            cx.background_executor().timer(Duration::from_millis(300)).await;
            let _ = cx.update(|window, _| window.remove_window());
        })
        .detach();
}

impl Render for Page {
    fn render(&mut self, window: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        let s = window.viewport_size();
        let now = (f32::from(s.width), f32::from(s.height));
        if self.laid.replace(now) != now
            && let Some(v) = &self.view
        {
            let _ = v.set_bounds(fill(window));
        }
        div().size_full().bg(rgb(0x000000))
    }
}

fn fill(window: &Window) -> wry::Rect {
    let s = window.viewport_size();
    wry::Rect {
        position: wry::dpi::LogicalPosition::new(0., 0.).into(),
        size: wry::dpi::LogicalSize::new(f64::from(f32::from(s.width)), f64::from(f32::from(s.height))).into(),
    }
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

/// The store every non-isolated app shares: the Tauri client's (WebView2 under
/// app_local_data_dir), so their storage carries over. macOS keys its default store on the
/// bundle identifier, which the two clients share.
#[cfg(windows)]
fn shared_context() -> &'static mut WebContext {
    thread_local! {
        static SHARED: Cell<*mut WebContext> = const { Cell::new(std::ptr::null_mut()) };
    }
    SHARED.with(|c| {
        if c.get().is_null() {
            let dir = dirs::data_local_dir().map(|d| d.join("io.vectorapp"));
            c.set(Box::into_raw(Box::new(WebContext::new(dir))));
        }
        // SAFETY: leaked once on the UI thread; each builder's borrow ends at its build.
        unsafe { &mut *c.get() }
    })
}

fn webview(spec: &Spec, window: &Window) -> Result<(wry::WebView, Option<WebContext>), String> {
    let first = match &spec.isolated {
        Some(i) => i.boot_url.clone(),
        None => spec.url.clone(),
    };
    let (own_scheme, own_host) = origin(&first);
    let ipc_label = spec.label.clone();
    let scheme_label = spec.label.clone();
    #[cfg(windows)]
    let mut context = spec.isolated.as_ref().map(|i| WebContext::new(Some(i.data_dir.clone())));
    #[cfg(not(windows))]
    let mut context: Option<WebContext> = None;
    let mut builder = match context.as_mut() {
        Some(c) => WebViewBuilder::new_with_web_context(c),
        #[cfg(windows)]
        None => WebViewBuilder::new_with_web_context(shared_context()),
        #[cfg(not(windows))]
        None => WebViewBuilder::new(),
    };
    #[cfg(target_os = "macos")]
    if spec.isolated.is_some() {
        use wry::WebViewBuilderExtDarwin;
        let store = spec.isolated.as_ref().and_then(|i| i.data_dir.file_name()).map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        builder = match vector_media::miniapps::isolated::store_identifier(&store) {
            Some(id) => builder.with_data_store_identifier(id),
            None => builder.with_incognito(true),
        };
    }
    builder = builder
        .with_initialization_script_for_main_only(spec.init.clone(), false)
        .with_asynchronous_custom_protocol("webxdc".into(), move |_, request, responder| {
            vector_media::miniapps::serve(&scheme_label, request, move |response| responder.respond(response));
        })
        .with_ipc_handler(move |request| emit(Event::Ipc(ipc_label.clone(), request.into_body())))
        .with_navigation_handler(move |url| {
            let ok = origin(&url) == (own_scheme.clone(), own_host.clone());
            if !ok {
                vector_core::log_warn!("[MiniApp] blocked navigation to {url}");
            }
            ok
        })
        .with_devtools(cfg!(debug_assertions))
        .with_focused(true)
        .with_bounds(fill(window))
        .with_url(&first);
    let view = builder.build_as_child(window).map_err(|e| e.to_string())?;
    Ok((view, context))
}

fn build(spec: Spec, cx: &mut App) -> Result<(), String> {
    let opts = WindowOptions {
        titlebar: Some(TitlebarOptions { title: Some(spec.title.clone().into()), ..Default::default() }),
        window_bounds: Some(WindowBounds::Windowed(Bounds::centered(None, size(px(480.), px(640.)), cx))),
        window_min_size: Some(size(px(320.), px(480.))),
        ..Default::default()
    };
    let mut failed = None;
    let label = spec.label.clone();
    let isolated = spec.isolated.as_ref().map(|i| i.id);
    let handle = cx
        .open_window(opts, |window, cx| {
            let made = webview(&spec, window);
            let (view, context) = match made {
                Ok((v, c)) => (Some(v), c),
                Err(e) => {
                    failed = Some(e);
                    (None, None)
                }
            };
            let laid = Cell::new((f32::from(window.viewport_size().width), f32::from(window.viewport_size().height)));
            cx.new(|cx| {
                let label = label.clone();
                cx.on_release(move |_, _| {
                    if let Some(id) = isolated {
                        vector_media::miniapps::isolated::stop_if(&label, id);
                    }
                    OPEN.with(|o| o.borrow_mut().remove(&label));
                    emit(Event::Closed(label));
                })
                .detach();
                Page { view, _context: context, farewell: spec.farewell.clone(), leaving: Cell::new(false), laid }
            })
        })
        .map_err(|e| e.to_string())?;
    if let Some(e) = failed {
        let _ = handle.update(cx, |_, window, _| window.remove_window());
        return Err(e);
    }
    let _ = handle.update(cx, |_, window, cx| {
        window.on_window_should_close(cx, move |window, cx| {
            let Ok(page) = handle.read(cx) else { return true };
            if !page.farewell() {
                return true;
            }
            remove_soon(window, cx);
            false
        });
        window.activate_window();
    });
    OPEN.with(|o| o.borrow_mut().insert(spec.label, handle));
    Ok(())
}

pub fn player(spec: PlayerSpec) -> Result<(), String> {
    on_ui(move |cx| {
        if let Some(handle) = OPEN.with(|o| o.borrow().get(PLAYER).copied()) {
            let done = handle.update(cx, |page, window, _| {
                if let Some(v) = &page.view {
                    let _ = v.load_url(&spec.url);
                }
                window.set_window_title(&spec.title);
                window.resize(size(px(spec.size.0), px(spec.size.1)));
                window.activate_window();
            });
            if done.is_ok() {
                return;
            }
        }
        if let Err(e) = build_player(spec, cx) {
            vector_core::log_warn!("[Player] window failed: {e}");
            emit(Event::Failed(PLAYER.into(), e));
        }
    })
}

fn build_player(spec: PlayerSpec, cx: &mut App) -> Result<(), String> {
    let opts = WindowOptions {
        titlebar: Some(TitlebarOptions { title: Some(spec.title.clone().into()), ..Default::default() }),
        window_bounds: Some(WindowBounds::Windowed(Bounds::centered(None, size(px(spec.size.0), px(spec.size.1)), cx))),
        ..Default::default()
    };
    let mut failed = None;
    let handle = cx
        .open_window(opts, |window, cx| {
            let own = origin(&spec.url);
            let made = WebViewBuilder::new()
                .with_incognito(true)
                .with_initialization_script_for_main_only(super::PLAYER_SHIM.to_string(), true)
                .with_ipc_handler(|request| emit(Event::Player(request.into_body())))
                .with_navigation_handler(move |url| origin(&url) == own)
                .with_new_window_req_handler(|url, _| {
                    emit(Event::Browse(url));
                    wry::NewWindowResponse::Deny
                })
                .with_bounds(fill(window))
                .with_url(&spec.url)
                .build_as_child(window);
            let view = made.map_err(|e| failed = Some(e.to_string())).ok();
            let laid = Cell::new((f32::from(window.viewport_size().width), f32::from(window.viewport_size().height)));
            cx.new(|cx| {
                cx.on_release(|_, _| {
                    OPEN.with(|o| o.borrow_mut().remove(PLAYER));
                    emit(Event::Closed(PLAYER.into()));
                })
                .detach();
                Page { view, _context: None, farewell: String::new(), leaving: Cell::new(true), laid }
            })
        })
        .map_err(|e| e.to_string())?;
    if let Some(e) = failed {
        let _ = handle.update(cx, |_, window, _| window.remove_window());
        return Err(e);
    }
    let _ = handle.update(cx, |_, window, _| window.activate_window());
    OPEN.with(|o| o.borrow_mut().insert(PLAYER.into(), handle));
    Ok(())
}

pub fn open(spec: Spec) -> Result<(), String> {
    on_ui(move |cx| {
        let label = spec.label.clone();
        if let Err(e) = build(spec, cx) {
            vector_core::log_warn!("[MiniApp] window failed: {e}");
            emit(Event::Failed(label, e));
        }
    })
}

fn with(label: String, f: impl FnOnce(&mut Page, &mut Window, &mut App) + Send + 'static) {
    let _ = on_ui(move |cx| {
        let Some(handle) = OPEN.with(|o| o.borrow().get(&label).copied()) else { return };
        let _ = handle.update(cx, |page, window, cx| f(page, window, cx));
    });
}

pub fn focus(label: &str, navigate: Option<String>) {
    with(label.to_string(), move |page, window, _| {
        if let (Some(url), Some(v)) = (navigate, &page.view) {
            let _ = v.load_url(&url);
        }
        window.activate_window();
    });
}

pub fn eval(label: &str, js: String) {
    with(label.to_string(), move |page, _, _| {
        if let Some(v) = &page.view {
            let _ = v.evaluate_script(&js);
        }
    });
}

/// Gone at once, with no farewell page: an account swap.
pub fn destroy(label: &str) {
    with(label.to_string(), |_, window, _| window.remove_window());
}

pub fn close(label: &str) {
    with(label.to_string(), |page, window, cx| page.leave(window, cx));
}
