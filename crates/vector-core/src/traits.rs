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
