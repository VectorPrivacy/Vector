//! TalkBack: GPUI's AccessKit tree as virtual views of `GpuiView`, through accesskit_android's
//! low-level adapter. Android asks on its UI thread; GPUI's updates queue their events, which
//! the UI thread raises, since raising can call back into the adapter.

use crate::bridge;
use accesskit::{ActionHandler, ActionRequest, ActivationHandler, TreeUpdate};
use accesskit_android::{Adapter, PlatformAction, QueuedEvents};
use gpui::A11yCallbacks;
use jni::JNIEnv;
use jni::objects::{JClass, JObject};
use jni::sys::{JNI_FALSE, JNI_TRUE, jboolean, jfloat, jint, jobject};
use std::sync::{LazyLock, Mutex, MutexGuard};

#[derive(Default)]
struct Shared {
    adapter: Adapter,
    callbacks: Option<A11yCallbacks>,
    /// Android asked before GPUI's window existed, so the adapter holds a placeholder tree.
    placeholder: bool,
    queued: Vec<QueuedEvents>,
}

static SHARED: LazyLock<Mutex<Shared>> = LazyLock::new(Mutex::default);

fn shared() -> MutexGuard<'static, Shared> {
    SHARED.lock().unwrap_or_else(|e| e.into_inner())
}

struct Activation<'a> {
    callbacks: Option<&'a A11yCallbacks>,
    placeholder: &'a mut bool,
}

impl ActivationHandler for Activation<'_> {
    fn request_initial_tree(&mut self) -> Option<TreeUpdate> {
        let tree = self.callbacks.and_then(|c| (c.activation)());
        *self.placeholder = tree.is_none();
        tree
    }
}

struct Action<'a>(Option<&'a A11yCallbacks>);

impl ActionHandler for Action<'_> {
    fn do_action(&mut self, request: ActionRequest) {
        if let Some(callbacks) = self.0 {
            (callbacks.action)(request);
        }
    }
}

/// From GPUI's thread, when its window opens.
pub(crate) fn init(callbacks: A11yCallbacks) {
    let mut shared = shared();
    shared.callbacks = Some(callbacks);
    // A placeholder never activates GPUI, so the next ask must start over.
    if std::mem::take(&mut shared.placeholder) {
        shared.adapter = Adapter::default();
        drop(shared);
        bridge::a11y_changed();
    }
}

/// From GPUI's thread, after each frame while a screen reader is on.
pub(crate) fn update(tree: TreeUpdate) {
    let mut shared = shared();
    let Some(events) = shared.adapter.update_if_active(|| tree) else { return };
    shared.queued.push(events);
    let first = shared.queued.len() == 1;
    drop(shared);
    if first {
        bridge::a11y_raise();
    }
}

fn with_adapter<R>(f: impl FnOnce(&mut Adapter, &mut Activation, &mut Action) -> R) -> R {
    let mut shared = shared();
    let Shared { adapter, callbacks, placeholder, .. } = &mut *shared;
    let mut activation = Activation { callbacks: callbacks.as_ref(), placeholder };
    let mut action = Action(callbacks.as_ref());
    f(adapter, &mut activation, &mut action)
}

/// A panic cannot unwind into Java: it would abort the process.
fn guarded<R>(fallback: R, f: impl FnOnce() -> R) -> R {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(f)).unwrap_or_else(|_| {
        log::error!("gpui_android: accessibility panicked");
        fallback
    })
}

fn raised(events: Option<QueuedEvents>, env: &mut JNIEnv, host: &JObject) -> jboolean {
    match events {
        Some(events) => {
            events.raise(env, host);
            JNI_TRUE
        }
        None => JNI_FALSE,
    }
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_io_vectorapp_gpui_Bridge_nativeA11yNode<'l>(
    mut env: JNIEnv<'l>,
    _class: JClass<'l>,
    host: JObject<'l>,
    id: jint,
) -> jobject {
    guarded(std::ptr::null_mut(), || {
        with_adapter(|adapter, activation, _| adapter.create_accessibility_node_info(activation, &mut env, &host, id))
            .into_raw()
    })
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_io_vectorapp_gpui_Bridge_nativeA11yFocus<'l>(
    mut env: JNIEnv<'l>,
    _class: JClass<'l>,
    host: JObject<'l>,
    kind: jint,
) -> jobject {
    guarded(std::ptr::null_mut(), || {
        with_adapter(|adapter, activation, _| adapter.find_focus(activation, &mut env, &host, kind)).into_raw()
    })
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_io_vectorapp_gpui_Bridge_nativeA11yAction<'l>(
    mut env: JNIEnv<'l>,
    _class: JClass<'l>,
    host: JObject<'l>,
    id: jint,
    action: jint,
    arguments: JObject<'l>,
) -> jboolean {
    guarded(JNI_FALSE, || {
        let Some(platform_action) = PlatformAction::from_java(&mut env, action, &arguments) else {
            return JNI_FALSE;
        };
        let events = with_adapter(|adapter, _, handler| adapter.perform_action(handler, id, &platform_action));
        raised(events, &mut env, &host)
    })
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_io_vectorapp_gpui_Bridge_nativeA11yHover<'l>(
    mut env: JNIEnv<'l>,
    _class: JClass<'l>,
    host: JObject<'l>,
    action: jint,
    x: jfloat,
    y: jfloat,
) -> jboolean {
    guarded(JNI_FALSE, || {
        let events = with_adapter(|adapter, activation, _| adapter.on_hover_event(activation, action, x, y));
        raised(events, &mut env, &host)
    })
}

/// The events GPUI's updates queued, on the UI thread.
#[unsafe(no_mangle)]
pub extern "system" fn Java_io_vectorapp_gpui_Bridge_nativeA11yRaise<'l>(
    mut env: JNIEnv<'l>,
    _class: JClass<'l>,
    host: JObject<'l>,
) {
    let queued = std::mem::take(&mut shared().queued);
    guarded((), || {
        for events in queued {
            events.raise(&mut env, &host);
        }
    });
}

/// Every accessibility service went off: GPUI stops building trees until one asks again.
#[unsafe(no_mangle)]
pub extern "system" fn Java_io_vectorapp_gpui_Bridge_nativeA11yOff<'l>(_env: JNIEnv<'l>, _class: JClass<'l>) {
    let mut shared = shared();
    shared.adapter = Adapter::default();
    shared.placeholder = false;
    shared.queued.clear();
    if let Some(callbacks) = &shared.callbacks {
        (callbacks.deactivation)();
    }
}
