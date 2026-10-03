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

#[cfg(test)]
mod clock_audit {
    /// `std::time::{SystemTime, Instant}` panic on wasm32 ("time not implemented on this
    /// platform"), so Vector Web dies wherever one runs. Code shared with the web reads the clock
    /// through `web_time` or `rt::time`; test modules may use std.
    #[test]
    fn shared_code_never_reads_the_std_clock() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
        let mut offenders = Vec::new();
        let mut scanned = 0usize;
        let mut stack = vec![root.join("src"), root.join("../vector-web/src")];
        while let Some(path) = stack.pop() {
            if path.is_dir() {
                stack.extend(std::fs::read_dir(&path).into_iter().flatten().flatten().map(|e| e.path()));
                continue;
            }
            if path.extension().is_none_or(|e| e != "rs") {
                continue;
            }
            let Ok(src) = std::fs::read_to_string(&path) else { continue };
            scanned += 1;
            // Inside a `#[cfg(test)]` item: the depth its braces must close back to.
            let (mut depth, mut skip_to): (i64, Option<i64>) = (0, None);
            let mut pending_test = false;
            // Inside a `use std::time::{ ... }` that spans lines.
            let mut in_time_use = false;
            for (i, line) in src.lines().enumerate() {
                let code = line.split("//").next().unwrap_or("");
                if code.trim_start().starts_with("#[cfg(test)]") {
                    pending_test = true;
                    continue;
                }
                let before = depth;
                depth += code.matches('{').count() as i64 - code.matches('}').count() as i64;
                if pending_test && !code.trim().is_empty() && !code.trim_start().starts_with("#[") {
                    // The first real line after the attribute starts the test item: a block to skip,
                    // or a one-line item (`static ..;`, `fn f() { .. }`) that is test code by itself.
                    pending_test = false;
                    if depth > before {
                        skip_to = Some(before);
                    } else {
                        continue;
                    }
                }
                if let Some(level) = skip_to {
                    if depth <= level {
                        skip_to = None;
                    }
                    continue;
                }
                if code.contains("use std::time::{") || code.contains("use std::{") {
                    in_time_use = true;
                }
                let names_clock = code.contains("SystemTime") || code.contains("Instant");
                if code.contains("std::time::SystemTime") || code.contains("std::time::Instant")
                    || code.contains("use std::time::*")
                    || (code.contains("use std::time::") && names_clock)
                    || (in_time_use && names_clock && (code.contains("time::") || !code.contains("use std::{")))
                {
                    offenders.push(format!("{}:{}", path.display(), i + 1));
                }
                if in_time_use && code.contains('}') {
                    in_time_use = false;
                }
            }
        }
        assert!(scanned > 50, "only scanned {scanned} files: the trees moved");
        assert!(offenders.is_empty(), "std clock in code Vector Web runs, use web_time:\n  {}", offenders.join("\n  "));
    }
}
