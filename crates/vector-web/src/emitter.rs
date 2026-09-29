//! Backend → page events.

use std::cell::RefCell;

use wasm_bindgen::JsValue;

thread_local! {
    static SINK: RefCell<Option<js_sys::Function>> = const { RefCell::new(None) };
}

pub fn set_sink(sink: js_sys::Function) {
    SINK.with(|s| *s.borrow_mut() = Some(sink));
}

/// Hand an event to the page. Payload is JSON text, parsed on the other side.
pub fn emit_raw(event: &str, json: &str) {
    SINK.with(|s| {
        if let Some(f) = s.borrow().as_ref() {
            let _ = f.call2(&JsValue::NULL, &JsValue::from_str(event), &JsValue::from_str(json));
        }
    });
}

/// For events the Tauri shell emits directly rather than through core.
pub fn emit(event: &str, payload: &impl serde::Serialize) {
    if let Ok(json) = serde_json::to_string(payload) {
        emit_raw(event, &json);
    }
}

/// Core's emitter. The sink is thread-local: a worker has one thread.
pub struct WebEmitter;

impl vector_core::EventEmitter for WebEmitter {
    fn emit(&self, event: &str, payload: serde_json::Value) {
        emit_raw(event, &payload.to_string());
    }

    fn emit_json(&self, event: &str, payload: &serde_json::value::RawValue) {
        emit_raw(event, payload.get());
    }

    fn prefers_json(&self) -> bool {
        true
    }
}
