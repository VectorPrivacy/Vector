//! The desktop client's Android glue: the media crate's, and the content URIs a share or a
//! picker hands over.

pub use vector_media::android::*;

#[path = "../../../src-tauri/src/android/filesystem.rs"]
#[allow(dead_code)]
pub mod filesystem;

use std::path::PathBuf;
use std::sync::OnceLock;

/// A folder `Context` names, by its getter.
fn folder(getter: &str) -> Option<PathBuf> {
    utils::with_android_context(|env, ctx| {
        let err = |e: jni::errors::Error| format!("{e:?}");
        let dir = env.call_method(ctx, getter, "()Ljava/io/File;", &[]).and_then(|v| v.l()).map_err(err)?;
        let path = env.call_method(&dir, "getAbsolutePath", "()Ljava/lang/String;", &[]).and_then(|v| v.l()).map_err(err)?;
        let path: String = env.get_string(&jni::objects::JString::from(path)).map_err(err)?.into();
        Ok(PathBuf::from(path))
    })
    .ok()
}

/// The app's data folder: where the Tauri client keeps its accounts.
pub fn data_dir() -> Option<&'static PathBuf> {
    static DIR: OnceLock<Option<PathBuf>> = OnceLock::new();
    DIR.get_or_init(|| folder("getDataDir")).as_ref()
}

pub fn cache_dir() -> Option<&'static PathBuf> {
    static DIR: OnceLock<Option<PathBuf>> = OnceLock::new();
    DIR.get_or_init(|| folder("getCacheDir")).as_ref()
}
