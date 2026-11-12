//! Lets modules written for the Tauri shell compile unchanged into the GPUI client: events go to
//! whatever sink the host installs, and the async runtime is the host's own tokio runtime.

use std::marker::PhantomData;
use std::path::PathBuf;
use std::sync::OnceLock;

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

/// Where `app_local_data_dir` points: set once by the host.
pub fn set_local_data_dir(dir: PathBuf) {
    let _ = LOCAL_DATA.set(dir);
}

pub struct PathResolver;

impl PathResolver {
    pub fn app_local_data_dir(&self) -> Result<PathBuf, Error> {
        LOCAL_DATA.get().cloned().ok_or_else(|| Error("app data directory not set".into()))
    }
}

pub trait Manager<R: Runtime> {
    fn path(&self) -> PathResolver;
}

impl<R: Runtime> Manager<R> for AppHandle<R> {
    fn path(&self) -> PathResolver {
        PathResolver
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
