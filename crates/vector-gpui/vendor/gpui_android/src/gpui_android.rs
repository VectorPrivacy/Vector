//! GPUI on Android. The app's one window draws into the activity's `SurfaceView` through
//! wgpu; touches, keys, insets, the soft keyboard and system services pass through the
//! Kotlin class `io.vectorapp.gpui.Bridge`. GPUI runs on its own thread with an `ALooper`,
//! so the UI thread only forwards, and the app survives activity recreation.
//!
//! The app provides `#[unsafe(no_mangle)] fn gpui_android_main()`, which the bridge calls on
//! that thread once the activity starts, and which runs the app as a desktop `main` would.
#![cfg(target_os = "android")]

mod bridge;
mod dispatcher;
mod display;
mod keys;
mod platform;
mod text_input;
mod window;

use std::cell::{Cell, RefCell};

pub use bridge::{app_context, java_vm, with_env};
pub use platform::AndroidPlatform;

pub fn current_platform(headless: bool) -> std::rc::Rc<dyn gpui::Platform> {
    std::rc::Rc::new(AndroidPlatform::new(headless))
}

/// A callback that may replace or remove itself while it runs.
pub(crate) struct CallbackSlot<T> {
    value: RefCell<Option<T>>,
    generation: Cell<u64>,
}

impl<T> Default for CallbackSlot<T> {
    fn default() -> Self {
        Self {
            value: RefCell::new(None),
            generation: Cell::new(0),
        }
    }
}

impl<T> CallbackSlot<T> {
    pub(crate) fn set(&self, value: T) {
        self.generation.set(self.generation.get().wrapping_add(1));
        drop(self.value.replace(Some(value)));
    }

    pub(crate) fn take(&self) -> Option<T> {
        self.generation.set(self.generation.get().wrapping_add(1));
        self.value.borrow_mut().take()
    }

    pub(crate) fn with<R>(&self, callback: impl FnOnce(&mut T) -> R) -> Option<R> {
        let mut value = self.value.borrow_mut().take()?;
        let generation = self.generation.get();
        let result = callback(&mut value);
        // Not restored when the callback replaced or removed itself meanwhile.
        if self.generation.get() == generation {
            drop(self.value.replace(Some(value)));
        }
        Some(result)
    }
}
