//! The desktop client's Android glue: the media crate's, and the content URIs a share or a
//! picker hands over.

pub use vector_media::android::*;

#[path = "../../../src-tauri/src/android/filesystem.rs"]
#[allow(dead_code)]
pub mod filesystem;
