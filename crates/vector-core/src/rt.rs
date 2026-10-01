//! Task and timer facade: tokio natively, the browser event loop on wasm32.
//!
//! wasm32-unknown-unknown has no threads and no tokio driver, so spawns go to
//! `spawn_local` and timers to `setTimeout`. Signatures match tokio's so call
//! sites read the same on both targets.

#[cfg(not(target_arch = "wasm32"))]
pub use tokio::task::{spawn, spawn_blocking, yield_now, JoinError, JoinHandle};

#[cfg(not(target_arch = "wasm32"))]
pub mod time {
    pub use tokio::time::*;
}

#[cfg(target_arch = "wasm32")]
pub use wasm::*;

/// Whether work can be spawned from here: natively only inside a tokio
/// runtime (a drop off-runtime, or during shutdown, has none); always in the
/// browser, whose event loop is the runtime.
pub fn can_spawn() -> bool {
    #[cfg(not(target_arch = "wasm32"))]
    {
        tokio::runtime::Handle::try_current().is_ok()
    }
    #[cfg(target_arch = "wasm32")]
    {
        true
    }
}

#[cfg(target_arch = "wasm32")]
mod wasm {
    use futures_util::future::{AbortHandle, Abortable};
    use std::future::Future;
    use std::pin::Pin;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;
    use std::task::{Context, Poll};

    pub mod time {
        pub use std::time::Duration;
        pub use wasmtimer::std::Instant;
        pub use wasmtimer::tokio::{
            interval, interval_at, sleep, sleep_until, Interval, MissedTickBehavior, Sleep, Timeout,
        };

        // tokio's take `IntoFuture`, which nostr-sdk's request builders rely on.
        pub fn timeout<F: std::future::IntoFuture>(d: Duration, f: F) -> Timeout<F::IntoFuture> {
            wasmtimer::tokio::timeout(d, f.into_future())
        }

        pub fn timeout_at<F: std::future::IntoFuture>(at: Instant, f: F) -> Timeout<F::IntoFuture> {
            wasmtimer::tokio::timeout_at(at, f.into_future())
        }
        pub mod error {
            pub use wasmtimer::tokio::error::Elapsed;
        }
    }

    #[derive(Debug)]
    pub struct JoinError;

    impl JoinError {
        pub fn is_cancelled(&self) -> bool { true }
        pub fn is_panic(&self) -> bool { false }
    }

    impl std::fmt::Display for JoinError {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.write_str("task was cancelled")
        }
    }

    impl std::error::Error for JoinError {}

    pub struct JoinHandle<T> {
        rx: tokio::sync::oneshot::Receiver<T>,
        abort: AbortHandle,
        done: Arc<AtomicBool>,
    }

    impl<T> JoinHandle<T> {
        pub fn abort(&self) { self.abort.abort(); }
        pub fn is_finished(&self) -> bool { self.done.load(Ordering::Relaxed) || self.abort.is_aborted() }
    }

    impl<T> Future for JoinHandle<T> {
        type Output = Result<T, JoinError>;
        fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
            Pin::new(&mut self.rx).poll(cx).map(|r| r.map_err(|_| JoinError))
        }
    }

    pub fn spawn<F>(fut: F) -> JoinHandle<F::Output>
    where
        F: Future + 'static,
        F::Output: 'static,
    {
        let (tx, rx) = tokio::sync::oneshot::channel();
        let (abort, reg) = AbortHandle::new_pair();
        let done = Arc::new(AtomicBool::new(false));
        let flag = done.clone();
        wasm_bindgen_futures::spawn_local(async move {
            if let Ok(v) = Abortable::new(fut, reg).await {
                let _ = tx.send(v);
            }
            flag.store(true, Ordering::Relaxed);
        });
        JoinHandle { rx, abort, done }
    }

    /// No worker pool: the closure runs on the next tick of the event loop.
    pub fn spawn_blocking<F, R>(f: F) -> JoinHandle<R>
    where
        F: FnOnce() -> R + 'static,
        R: 'static,
    {
        spawn(async move { f() })
    }

    pub async fn yield_now() {
        struct YieldNow(bool);
        impl Future for YieldNow {
            type Output = ();
            fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<()> {
                if self.0 {
                    return Poll::Ready(());
                }
                self.0 = true;
                cx.waker().wake_by_ref();
                Poll::Pending
            }
        }
        YieldNow(false).await
    }
}

/// `Send` natively; nothing on wasm32, where there is one thread and browser
/// futures hold `!Send` JS handles.
#[cfg(not(target_arch = "wasm32"))]
pub trait MaybeSend: Send {}
#[cfg(not(target_arch = "wasm32"))]
impl<T: Send + ?Sized> MaybeSend for T {}

#[cfg(target_arch = "wasm32")]
pub trait MaybeSend {}
#[cfg(target_arch = "wasm32")]
impl<T: ?Sized> MaybeSend for T {}

/// Wall clock and `Instant` that work on both targets, for `#[macro_export]`
/// macros that expand in crates without a `web-time` dependency.
pub use web_time as time_std;
