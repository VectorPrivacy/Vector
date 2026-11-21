//! The phone's own tilt for the badge card (`gpui/Tilt.kt`), in the card's -1..1 units.
//! Elsewhere, and on a phone without the sensor, the pointer keeps it.

#[cfg(target_os = "android")]
mod phone {
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicBool, Ordering};

    use jni::objects::{JClass, JObject, JValue};
    use jni::JNIEnv;

    use crate::android::utils::{jni_fence, with_android_context};

    static WANTED: AtomicBool = AtomicBool::new(false);
    static LIVE: AtomicBool = AtomicBool::new(false);
    static LAST: Mutex<Option<(f32, f32)>> = Mutex::new(None);

    /// The card's most tilted, in degrees, as Tauri's `_BADGE_TILT_DEG`.
    const MAX_DEG: f32 = 12.;

    fn class<'a>(env: &mut JNIEnv<'a>, ctx: &JObject<'a>) -> Result<JClass<'a>, String> {
        let err = |e: jni::errors::Error| format!("{e:?}");
        let loader = env.call_method(ctx, "getClassLoader", "()Ljava/lang/ClassLoader;", &[]).and_then(|v| v.l()).map_err(err)?;
        let name = env.new_string("io.vectorapp.gpui.Tilt").map_err(err)?;
        let class = env
            .call_method(&loader, "loadClass", "(Ljava/lang/String;)Ljava/lang/Class;", &[JValue::Object(&name)])
            .and_then(|v| v.l())
            .map_err(err)?;
        Ok(JClass::from(class))
    }

    pub fn want(on: bool) {
        if WANTED.swap(on, Ordering::AcqRel) == on {
            return;
        }
        *LAST.lock().unwrap_or_else(|e| e.into_inner()) = None;
        let out = with_android_context(|env, ctx| {
            let class = class(env, ctx)?;
            let err = |e: jni::errors::Error| format!("{e:?}");
            if on {
                env.call_static_method(&class, "start", "(Landroid/content/Context;)Z", &[JValue::Object(ctx)])
                    .and_then(|v| v.z())
                    .map_err(err)
            } else {
                env.call_static_method(&class, "stop", "()V", &[]).map(|_| false).map_err(err)
            }
        });
        if let Err(e) = &out {
            log::warn!("tilt: {e}");
        }
        LIVE.store(out.unwrap_or(false), Ordering::Release);
    }

    /// `(x, y)` for the card, or None while the sensor isn't driving it.
    pub fn current() -> Option<(f32, f32)> {
        if !LIVE.load(Ordering::Acquire) {
            return None;
        }
        Some(LAST.lock().unwrap_or_else(|e| e.into_inner()).unwrap_or_default())
    }

    #[unsafe(no_mangle)]
    pub extern "C" fn Java_io_vectorapp_gpui_Tilt_nativeTilt(_env: JNIEnv, _class: JClass, rx: f32, ry: f32) {
        jni_fence("Java_io_vectorapp_gpui_Tilt_nativeTilt", || (), move || {
            if !WANTED.load(Ordering::Acquire) {
                return;
            }
            *LAST.lock().unwrap_or_else(|e| e.into_inner()) = Some(super::card(rx, ry, MAX_DEG));
            crate::backend::emit("badge_tilt", serde_json::Value::Null);
        })
    }
}

#[cfg(target_os = "android")]
pub use phone::{current, want};

#[cfg(not(target_os = "android"))]
pub fn want(_on: bool) {}

#[cfg(not(target_os = "android"))]
pub fn current() -> Option<(f32, f32)> {
    None
}

/// Pitch and roll in degrees to the card's tilt: x leans with the roll, y against the pitch.
#[cfg_attr(not(target_os = "android"), allow(dead_code))]
fn card(rx: f32, ry: f32, max: f32) -> (f32, f32) {
    ((ry / max).clamp(-1., 1.), (-rx / max).clamp(-1., 1.))
}

#[cfg(test)]
mod tests {
    #[test]
    fn degrees_map_to_the_pointer_units() {
        assert_eq!(super::card(0., 6., 12.), (0.5, 0.));
        assert_eq!(super::card(12., 0., 12.), (0., -1.));
        assert_eq!(super::card(-30., 30., 12.), (1., 1.));
    }
}
