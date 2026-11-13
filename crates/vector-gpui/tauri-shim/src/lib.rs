//! Lets modules written for the Tauri shell compile unchanged into the GPUI client: events go to
//! whatever sink the host installs, and the async runtime is the host's own tokio runtime.

use std::marker::PhantomData;
use std::path::PathBuf;
use std::sync::OnceLock;

pub use url::Url;
pub use vector_tauri_shim_macros::command;

#[derive(Debug)]
pub struct Error(pub String);

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for Error {}

pub trait Runtime: 'static {}

/// The one runtime there is.
pub struct Wry;
impl Runtime for Wry {}

/// The host's event sink, standing in for the Tauri app.
pub struct AppHandle<R: Runtime = Wry> {
    sink: fn(&str, serde_json::Value),
    _runtime: PhantomData<fn() -> R>,
}

impl<R: Runtime> Clone for AppHandle<R> {
    fn clone(&self) -> Self {
        *self
    }
}
impl<R: Runtime> Copy for AppHandle<R> {}

impl AppHandle {
    pub const fn new(sink: fn(&str, serde_json::Value)) -> Self {
        Self { sink, _runtime: PhantomData }
    }
}

static LOCAL_DATA: OnceLock<PathBuf> = OnceLock::new();
static DATA: OnceLock<PathBuf> = OnceLock::new();

/// Where `app_data_dir` points: set once by the host.
pub fn set_data_dir(dir: PathBuf) {
    let _ = DATA.set(dir);
}

/// Where `app_local_data_dir` points: set once by the host.
pub fn set_local_data_dir(dir: PathBuf) {
    let _ = LOCAL_DATA.set(dir);
}

pub struct PathResolver;

impl PathResolver {
    pub fn app_data_dir(&self) -> Result<PathBuf, Error> {
        DATA.get().cloned().ok_or_else(|| Error("app data directory not set".into()))
    }
    pub fn app_local_data_dir(&self) -> Result<PathBuf, Error> {
        LOCAL_DATA.get().cloned().ok_or_else(|| Error("app data directory not set".into()))
    }
}

pub trait Manager<R: Runtime> {
    fn path(&self) -> PathResolver;
    /// The host's one window, whatever the label.
    fn get_webview_window(&self, _label: &str) -> Option<WebviewWindow> {
        WINDOW.get().map(|hooks| WebviewWindow(hooks))
    }
    /// What the host handed to [`manage`]; panics when it never did, as Tauri does.
    fn state<T: Send + Sync + 'static>(&self) -> State<'static, T> {
        State(managed::<T>().expect("tauri shim: state not managed"))
    }
}

impl<R: Runtime> Manager<R> for AppHandle<R> {
    fn path(&self) -> PathResolver {
        PathResolver
    }
}

/// What the host's window answers and does for modules that ask Tauri's.
pub struct WindowHooks {
    pub focused: fn() -> bool,
    /// Shown, unminimized and focused.
    pub reveal: fn(),
}

static WINDOW: OnceLock<WindowHooks> = OnceLock::new();

pub fn set_window(hooks: WindowHooks) {
    let _ = WINDOW.set(hooks);
}

pub struct WebviewWindow(&'static WindowHooks);

impl WebviewWindow {
    pub fn is_focused(&self) -> Result<bool, Error> {
        Ok((self.0.focused)())
    }
    pub fn show(&self) -> Result<(), Error> {
        (self.0.reveal)();
        Ok(())
    }
    pub fn unminimize(&self) -> Result<(), Error> {
        Ok(())
    }
    pub fn set_focus(&self) -> Result<(), Error> {
        Ok(())
    }
}

/// The bundle's identity, as `tauri.conf.json` gives it.
pub struct Config {
    pub identifier: String,
    pub product_name: Option<String>,
}

static CONFIG: OnceLock<Config> = OnceLock::new();

pub fn set_config(config: Config) {
    let _ = CONFIG.set(config);
}

impl<R: Runtime> AppHandle<R> {
    pub fn config(&self) -> &'static Config {
        CONFIG.get_or_init(|| Config { identifier: "io.vectorapp".into(), product_name: Some("Vector".into()) })
    }
}

/// `tauri-plugin-notification`'s plain notification, for a platform with no native backend:
/// the GPUI client has no plugin to fall back on, so it shows nothing.
pub trait NotificationExt {
    fn notification(&self) -> Notifications {
        Notifications
    }
}

impl<R: Runtime> NotificationExt for AppHandle<R> {}

pub struct Notifications;

impl Notifications {
    pub fn builder(&self) -> NotificationBuilder {
        NotificationBuilder
    }
}

pub struct NotificationBuilder;

impl NotificationBuilder {
    pub fn title(self, _: &str) -> Self {
        self
    }
    pub fn body(self, _: &str) -> Self {
        self
    }
    pub fn large_body(self, _: &str) -> Self {
        self
    }
    pub fn show(self) -> Result<(), Error> {
        Err(Error("no notification backend on this platform".into()))
    }
}

static MANAGED: std::sync::Mutex<Vec<(std::any::TypeId, &'static (dyn std::any::Any + Send + Sync))>> =
    std::sync::Mutex::new(Vec::new());

/// Process-lifetime state, one value a type, as `Builder::manage` registers it.
pub fn manage<T: Send + Sync + 'static>(value: T) -> &'static T {
    let mut all = MANAGED.lock().unwrap_or_else(|e| e.into_inner());
    if let Some((_, v)) = all.iter().find(|(t, _)| *t == std::any::TypeId::of::<T>()) {
        return v.downcast_ref().expect("same type");
    }
    let leaked: &'static T = Box::leak(Box::new(value));
    all.push((std::any::TypeId::of::<T>(), leaked));
    leaked
}

pub fn managed<T: Send + Sync + 'static>() -> Option<&'static T> {
    let all = MANAGED.lock().unwrap_or_else(|e| e.into_inner());
    all.iter()
        .find(|(t, _)| *t == std::any::TypeId::of::<T>())
        .and_then(|(_, v)| v.downcast_ref())
}

pub struct State<'a, T>(&'a T);

impl<T> std::ops::Deref for State<'_, T> {
    type Target = T;
    fn deref(&self) -> &T {
        self.0
    }
}

/// A webview's custom-scheme request: which webview asked.
pub struct UriSchemeContext<'a, R: Runtime = Wry> {
    label: &'a str,
    app: AppHandle<R>,
}

impl<'a, R: Runtime> UriSchemeContext<'a, R> {
    pub fn new(label: &'a str, app: AppHandle<R>) -> Self {
        Self { label, app }
    }
    pub fn webview_label(&self) -> &str {
        self.label
    }
    pub fn app_handle(&self) -> &AppHandle<R> {
        &self.app
    }
}

type Respond = Box<dyn FnOnce(http::Response<std::borrow::Cow<'static, [u8]>>) + Send>;

/// Answers a custom-scheme request, from any thread.
pub struct UriSchemeResponder(Respond);

impl UriSchemeResponder {
    pub fn new(f: impl FnOnce(http::Response<std::borrow::Cow<'static, [u8]>>) + Send + 'static) -> Self {
        Self(Box::new(f))
    }
    pub fn respond<T: Into<std::borrow::Cow<'static, [u8]>>>(self, response: http::Response<T>) {
        (self.0)(response.map(Into::into))
    }
}

pub mod ipc {
    use std::marker::PhantomData;
    use std::sync::Arc;

    /// A stream of messages to one webview's JS callback.
    pub struct Channel<T> {
        sink: Arc<dyn Fn(serde_json::Value) -> bool + Send + Sync>,
        _t: PhantomData<fn(T)>,
    }

    impl<T> Clone for Channel<T> {
        fn clone(&self) -> Self {
            Self { sink: self.sink.clone(), _t: PhantomData }
        }
    }

    impl<T: serde::Serialize> Channel<T> {
        /// `sink` answers false once its webview is gone.
        pub fn new(sink: impl Fn(serde_json::Value) -> bool + Send + Sync + 'static) -> Self {
            Self { sink: Arc::new(sink), _t: PhantomData }
        }
        pub fn send(&self, data: T) -> Result<(), super::Error> {
            let v = serde_json::to_value(data).map_err(|e| super::Error(e.to_string()))?;
            if (self.sink)(v) { Ok(()) } else { Err(super::Error("webview closed".into())) }
        }
    }
}

pub trait Emitter {
    fn emit<S: serde::Serialize + Clone>(&self, event: &str, payload: S) -> Result<(), Error>;
}

impl<R: Runtime> Emitter for AppHandle<R> {
    fn emit<S: serde::Serialize + Clone>(&self, event: &str, payload: S) -> Result<(), Error> {
        let value = serde_json::to_value(payload).map_err(|e| Error(e.to_string()))?;
        (self.sink)(event, value);
        Ok(())
    }
}

pub mod async_runtime {
    use super::OnceLock;
    use std::future::Future;

    static HANDLE: OnceLock<tokio::runtime::Handle> = OnceLock::new();

    /// Called once by the host before anything here runs.
    pub fn set(handle: tokio::runtime::Handle) {
        let _ = HANDLE.set(handle);
    }

    pub struct RuntimeHandle(&'static tokio::runtime::Handle);

    impl RuntimeHandle {
        pub fn inner(&self) -> &tokio::runtime::Handle {
            self.0
        }
    }

    fn get() -> &'static tokio::runtime::Handle {
        HANDLE.get().expect("tauri shim: runtime not set")
    }

    pub fn handle() -> RuntimeHandle {
        RuntimeHandle(get())
    }

    pub type JoinHandle<T> = tokio::task::JoinHandle<T>;

    pub fn spawn<F>(fut: F) -> JoinHandle<F::Output>
    where
        F: Future + Send + 'static,
        F::Output: Send + 'static,
    {
        get().spawn(fut)
    }

    pub fn block_on<F: Future>(fut: F) -> F::Output {
        get().block_on(fut)
    }
}
