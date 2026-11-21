//! What another app shares into Vector (Android's share sheet): held until the user picks a
//! chat, which then previews the file or takes the text into its composer.

use std::path::PathBuf;
use std::sync::Mutex;

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Shared {
    /// Readable by path: held in memory where the system allows, else copies in the cache.
    pub files: Vec<PathBuf>,
    pub text: String,
}

static PENDING: Mutex<Option<Shared>> = Mutex::new(None);

/// Keeps `shared` for the window, and tells it when it is up.
#[cfg_attr(not(target_os = "android"), allow(dead_code))]
pub fn arrived(shared: Shared) {
    if shared.files.is_empty() && shared.text.is_empty() {
        return;
    }
    *PENDING.lock().unwrap_or_else(|e| e.into_inner()) = Some(shared);
    crate::backend::emit("share_received", serde_json::Value::Null);
}

pub fn take() -> Option<Shared> {
    PENDING.lock().unwrap_or_else(|e| e.into_inner()).take()
}

#[cfg(target_os = "android")]
#[unsafe(no_mangle)]
pub extern "C" fn Java_io_vectorapp_MainActivity_nativeOnShare(
    mut env: jni::JNIEnv,
    _class: jni::objects::JClass,
    files: jni::objects::JObjectArray,
    text: jni::objects::JString,
) {
    crate::android::utils::jni_fence("Java_io_vectorapp_MainActivity_nativeOnShare", || (), move || {
        let n = env.get_array_length(&files).unwrap_or(0);
        let mut paths = Vec::new();
        for i in 0..n {
            let Ok(o) = env.get_object_array_element(&files, i) else { continue };
            if let Ok(s) = env.get_string(&jni::objects::JString::from(o)) {
                paths.push(PathBuf::from(String::from(s)));
            }
        }
        let text = env.get_string(&text).map(String::from).unwrap_or_default();
        arrived(Shared { files: paths, text });
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_share_waits_once_and_an_empty_one_is_dropped() {
        arrived(Shared::default());
        assert_eq!(take(), None);
        arrived(Shared { files: vec![], text: "hi".into() });
        assert_eq!(take().map(|s| s.text), Some("hi".into()));
        assert_eq!(take(), None);
    }
}
