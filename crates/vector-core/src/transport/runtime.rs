//! The transport runtime: the bridge, its connections and every long-lived transport socket live
//! here, so a short-lived caller runtime (Android background sync) can't take them down with it.

use std::sync::OnceLock;

static RT: OnceLock<tokio::runtime::Runtime> = OnceLock::new();

/// Lazily built and never shut down. A Clearnet-only process never builds it.
pub fn runtime() -> &'static tokio::runtime::Runtime {
    RT.get_or_init(|| {
        tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .thread_name("vector-net")
            .enable_all()
            .build()
            .expect("the transport runtime must start")
    })
}

/// The one way `transport/` and `i2p/` start tasks. Every call site says why it owns no account
/// state with a `spawn-detached:` marker.
pub fn spawn_on<F>(fut: F) -> tokio::task::JoinHandle<F::Output>
where
    F: std::future::Future + Send + 'static,
    F::Output: Send + 'static,
{
    // spawn-detached: the primitive itself; every caller carries its own marker.
    runtime().spawn(fut)
}

/// The runtime, only if something already needed it.
pub(crate) fn built() -> Option<&'static tokio::runtime::Runtime> {
    RT.get()
}
