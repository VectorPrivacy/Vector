//! The desktop client's image preparation (`shared/image.rs` and what it leans on), compiled
//! from its sources: uploads are resized, recompressed and stripped exactly as there.

#[path = "../../../src-tauri/src/shared/animated_webp.rs"]
pub mod animated_webp;
#[path = "../../../src-tauri/src/shared/cancel.rs"]
pub mod cancel;
#[path = "../../../src-tauri/src/shared/exif.rs"]
pub mod exif;
#[path = "../../../src-tauri/src/shared/image.rs"]
pub mod image;
