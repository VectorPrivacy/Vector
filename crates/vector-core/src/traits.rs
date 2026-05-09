//! Abstraction traits that decouple vector-core from any specific UI framework.
//!
//! Tauri, CLI, SDK, or any other frontend implements these traits to integrate
//! with vector-core. The core library never imports `tauri` directly.

use std::sync::OnceLock;

/// Emits events to the UI layer (Tauri frontend, CLI output, SDK callbacks).
///
/// Tauri: wraps `AppHandle::emit(event, payload)`
/// CLI: logs to stdout or pushes to a channel
/// SDK: invokes user-provided callbacks
pub trait EventEmitter: Send + Sync + 'static {
    fn emit(&self, event: &str, payload: serde_json::Value);
}

/// A no-op emitter for headless/test contexts.
pub struct NoOpEmitter;

impl EventEmitter for NoOpEmitter {
    fn emit(&self, _event: &str, _payload: serde_json::Value) {}
}

/// Global event emitter — set once by the integrator during initialization.
static EVENT_EMITTER: OnceLock<Box<dyn EventEmitter>> = OnceLock::new();

/// Register the global event emitter. Call once during app startup.
pub fn set_event_emitter(emitter: Box<dyn EventEmitter>) {
    let _ = EVENT_EMITTER.set(emitter);
}

/// Emit an event to the UI layer. No-op if no emitter is registered.
pub fn emit_event<T: serde::Serialize>(event: &str, payload: &T) {
    if let Some(emitter) = EVENT_EMITTER.get() {
        if let Ok(value) = serde_json::to_value(payload) {
            emitter.emit(event, value);
        }
    }
}

/// Emit a raw JSON value event to the UI layer.
pub fn emit_event_json(event: &str, payload: serde_json::Value) {
    if let Some(emitter) = EVENT_EMITTER.get() {
        emitter.emit(event, payload);
    }
}

/// Check if an event emitter is registered.
pub fn has_event_emitter() -> bool {
    EVENT_EMITTER.get().is_some()
}

/// Refreshes the integration layer's live MLS subscription set.
///
/// vector-core mutates the local "groups I'm in" set when accepting a
/// welcome, leaving, or being evicted. The integration layer (Tauri) keeps
/// a relay subscription whose `#h` filter list mirrors that set. Without
/// this hook, a kicked group stays subscribed and keeps receiving kind=445
/// events at epochs we don't have keys for — MDK marks them Failed and
/// they become permanently unrecoverable even after rejoin (MDK skips
/// Failed entries on retry).
///
/// Implementations typically spawn the async refresh on the host runtime;
/// the trait method is sync so vector-core can call it from anywhere.
pub trait SubscriptionRefresher: Send + Sync + 'static {
    fn refresh(&self);
}

pub struct NoOpSubscriptionRefresher;
impl SubscriptionRefresher for NoOpSubscriptionRefresher {
    fn refresh(&self) {}
}

static SUBSCRIPTION_REFRESHER: OnceLock<Box<dyn SubscriptionRefresher>> = OnceLock::new();

pub fn set_subscription_refresher(refresher: Box<dyn SubscriptionRefresher>) {
    let _ = SUBSCRIPTION_REFRESHER.set(refresher);
}

pub fn refresh_subscriptions() {
    if let Some(r) = SUBSCRIPTION_REFRESHER.get() {
        r.refresh();
    }
}

/// Trait for reporting download/upload progress.
pub trait ProgressReporter: Send + Sync {
    fn report_progress(&self, percentage: Option<u8>, bytes: Option<u64>, bytes_per_sec: Option<f64>) -> Result<(), &'static str>;
    fn report_complete(&self) -> Result<(), &'static str>;
}

/// A no-op progress reporter.
pub struct NoOpProgressReporter;

impl ProgressReporter for NoOpProgressReporter {
    fn report_progress(&self, _: Option<u8>, _: Option<u64>, _: Option<f64>) -> Result<(), &'static str> { Ok(()) }
    fn report_complete(&self) -> Result<(), &'static str> { Ok(()) }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    // ========================================================================
    // SubscriptionRefresher
    // ========================================================================

    /// Counter shared across all SubscriptionRefresher tests. We can't unset
    /// SUBSCRIPTION_REFRESHER (OnceLock) so all tests share the registered
    /// CountingRefresher; a test-serializer mutex + per-test counter reset
    /// gives us reliable observation of `refresh()` calls.
    static REFRESH_CALLS: AtomicUsize = AtomicUsize::new(0);
    static REFRESH_TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    struct CountingRefresher;
    impl SubscriptionRefresher for CountingRefresher {
        fn refresh(&self) {
            REFRESH_CALLS.fetch_add(1, Ordering::Relaxed);
        }
    }

    /// Register the CountingRefresher exactly once across all tests, then
    /// take the serial lock and zero the counter. Subsequent
    /// set_subscription_refresher calls are no-ops (OnceLock semantics) so
    /// CountingRefresher remains the live impl for every test in this
    /// process — that's deliberate and tested below.
    fn refresh_test_setup() -> std::sync::MutexGuard<'static, ()> {
        let guard = REFRESH_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        set_subscription_refresher(Box::new(CountingRefresher));
        REFRESH_CALLS.store(0, Ordering::Relaxed);
        guard
    }

    #[test]
    fn refresh_subscriptions_invokes_registered_impl_once() {
        let _g = refresh_test_setup();
        assert_eq!(REFRESH_CALLS.load(Ordering::Relaxed), 0);

        refresh_subscriptions();

        assert_eq!(REFRESH_CALLS.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn refresh_subscriptions_invokes_registered_impl_per_call() {
        // Each call MUST translate to one refresh invocation — the eviction
        // and accept_invite hooks both call refresh_subscriptions(), so any
        // accidental coalescing or guard-skip would break post-rejoin pushes.
        let _g = refresh_test_setup();

        for _ in 0..5 {
            refresh_subscriptions();
        }

        assert_eq!(REFRESH_CALLS.load(Ordering::Relaxed), 5);
    }

    #[test]
    fn set_subscription_refresher_is_idempotent() {
        // OnceLock semantics: only the first set wins. We register a SECOND
        // refresher (a panicking one) and verify the still-registered counter
        // is what gets called. Without OnceLock-set semantics, a runtime
        // re-registration could swap the live refresher out from under code
        // that's mid-flight.
        let _g = refresh_test_setup();

        struct PanicRefresher;
        impl SubscriptionRefresher for PanicRefresher {
            fn refresh(&self) {
                panic!("PanicRefresher::refresh() should never be reached — OnceLock should have ignored the second set_subscription_refresher call");
            }
        }
        set_subscription_refresher(Box::new(PanicRefresher));

        // The panic-refresher would crash the test if it had taken over.
        // Since CountingRefresher is still live, this just increments and
        // returns cleanly.
        refresh_subscriptions();

        assert_eq!(REFRESH_CALLS.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn refresh_subscriptions_is_thread_safe() {
        // The hook is called from cleanup_evicted_group which can run on any
        // thread (sync_blocking → block_on flow). Multiple refresh calls
        // racing must not lose any.
        let _g = refresh_test_setup();
        let handles: Vec<_> = (0..10).map(|_| {
            std::thread::spawn(|| {
                for _ in 0..100 {
                    refresh_subscriptions();
                }
            })
        }).collect();
        for h in handles { h.join().unwrap(); }

        assert_eq!(REFRESH_CALLS.load(Ordering::Relaxed), 1_000);
    }

    #[test]
    fn no_op_subscription_refresher_returns_quietly() {
        // Direct call to NoOpSubscriptionRefresher — must not panic, must not
        // increment the global counter (since it doesn't go through the hook).
        let _g = refresh_test_setup();
        let initial = REFRESH_CALLS.load(Ordering::Relaxed);
        let r = NoOpSubscriptionRefresher;
        r.refresh();
        r.refresh();
        assert_eq!(REFRESH_CALLS.load(Ordering::Relaxed), initial);
    }

    // ========================================================================
    // EventEmitter (existing pattern — light coverage for symmetry)
    // ========================================================================

    #[test]
    fn no_op_emitter_does_not_panic() {
        let e = NoOpEmitter;
        e.emit("test_event", serde_json::json!({"k": "v"}));
        // No assert — the contract is "doesn't panic, doesn't error".
    }

    #[test]
    fn emit_event_when_unregistered_is_silent() {
        // emit_event on a never-registered emitter must not panic. We can't
        // test the unregistered case after src-tauri's TauriEventEmitter has
        // been registered (OnceLock), but in pure-vector-core test runs no
        // emitter is registered, and the `if let Some(...)` guard handles it.
        // This test verifies the function call itself doesn't panic regardless.
        crate::traits::emit_event("test_event", &serde_json::json!({"k": "v"}));
    }

    #[test]
    fn emit_event_json_when_unregistered_is_silent() {
        crate::traits::emit_event_json("test_event", serde_json::json!({"k": "v"}));
    }

    #[test]
    fn no_op_progress_reporter_returns_ok() {
        let r = NoOpProgressReporter;
        assert!(r.report_progress(Some(50), Some(1024), Some(100.0)).is_ok());
        assert!(r.report_complete().is_ok());
    }
}
