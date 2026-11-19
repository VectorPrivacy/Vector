//! What the shared JNI modules take from the desktop client's background service: the process's
//! VM and application context, whether the activity is on screen, and avatars for the shade.
//! Message notifications come with the service itself.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::OnceLock;

use jni::objects::{GlobalRef, JObject};
use jni::sys::jboolean;
use jni::{JNIEnv, JavaVM};

pub static BG_JAVA_VM: OnceLock<JavaVM> = OnceLock::new();
pub static BG_APP_CONTEXT: OnceLock<GlobalRef> = OnceLock::new();
static FOREGROUND: AtomicBool = AtomicBool::new(false);

pub fn is_activity_in_foreground() -> bool {
    FOREGROUND.load(Ordering::Acquire)
}

pub fn post_notification_jni(
    _title: &str,
    _body: &str,
    _avatar_path: Option<&str>,
    _chat_id: Option<&str>,
    _sender_name: Option<&str>,
    _group_name: Option<&str>,
    _group_avatar_path: Option<&str>,
) {
}

pub fn cancel_notification_jni(_chat_id: &str) {}

/// A sealed cache file opened into a plain one the shade can read, kept until the source changes.
pub(crate) fn notification_image(path: Option<String>) -> Option<String> {
    let path = path?;
    let source = std::path::Path::new(&path);
    if !vector_core::vcache::is_sealed(source) {
        return Some(path);
    }
    let dir = vector_core::db::get_app_data_dir().ok()?.join("notifications").join("avatars");
    std::fs::create_dir_all(&dir).ok()?;
    let ext = source.extension().and_then(|e| e.to_str()).unwrap_or("img");
    let modified = std::fs::metadata(source).and_then(|m| m.modified()).ok()?;
    let stamp = modified.duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_nanos());
    let key = vector_core::crypto::sha256_hex(format!("{path}\n{stamp}").as_bytes());
    let out = dir.join(format!("{}.{ext}", &key[..32]));
    if !out.is_file() {
        let bytes = vector_core::vcache::read(source).ok()?;
        let tmp = dir.join(format!(".{}.{:08x}.tmp", &key[..32], rand::random::<u32>()));
        if std::fs::write(&tmp, &*bytes).and_then(|_| std::fs::rename(&tmp, &out)).is_err() {
            let _ = std::fs::remove_file(&tmp);
            return None;
        }
    }
    Some(out.to_string_lossy().into_owned())
}

/// `MainActivity.onCreate`: the activity for calls that need one, and the process's VM and
/// application context for the rest.
#[no_mangle]
pub extern "C" fn Java_io_vectorapp_MainActivity_nativeCreated(mut env: JNIEnv, activity: JObject) {
    super::utils::jni_fence("Java_io_vectorapp_MainActivity_nativeCreated", || (), move || {
        super::utils::set_activity(&mut env, &activity);
        if BG_JAVA_VM.get().is_none() {
            if let Ok(vm) = env.get_java_vm() {
                let _ = BG_JAVA_VM.set(vm);
            }
        }
        if BG_APP_CONTEXT.get().is_none() {
            let app = env.call_method(&activity, "getApplicationContext", "()Landroid/content/Context;", &[]).and_then(|v| v.l());
            match app.and_then(|app| env.new_global_ref(app)) {
                Ok(global) => {
                    let _ = BG_APP_CONTEXT.set(global);
                }
                Err(_) => {
                    let _ = env.exception_clear();
                }
            }
        }
    })
}

#[no_mangle]
pub extern "C" fn Java_io_vectorapp_MainActivity_nativeDestroyed(mut env: JNIEnv, activity: JObject) {
    super::utils::jni_fence("Java_io_vectorapp_MainActivity_nativeDestroyed", || (), move || {
        super::utils::clear_activity(&mut env, &activity);
    })
}

/// Resumed or paused: a notification for a chat on screen is never posted.
#[no_mangle]
pub extern "C" fn Java_io_vectorapp_MainActivity_nativeForeground(_env: JNIEnv, _activity: JObject, on: jboolean) {
    FOREGROUND.store(on != 0, Ordering::Release);
}
