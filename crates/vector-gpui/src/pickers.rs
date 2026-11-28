//! The file types each picker offers, as the Tauri client's dialogs filter them.

use gpui::SharedString;

pub type Filters = Option<Vec<(SharedString, Vec<SharedString>)>>;

fn only(name: SharedString, extensions: &[&'static str]) -> Filters {
    Some(vec![(name, extensions.iter().map(|e| SharedString::from(*e)).collect())])
}

/// `chat.js` `startWallpaperChange`.
pub fn wallpaper() -> Filters {
    only(crate::t::picker_filter_images(), &["png", "jpg", "jpeg", "webp", "gif", "svg"])
}

/// `main.js` `pickImage`: an avatar or banner, a Community's icon or banner.
pub fn picture() -> Filters {
    only(crate::t::picker_filter_image(), &["png", "jpg", "jpeg", "gif", "webp"])
}

/// `pack-creator.js`: a pack's emoji and its logo.
pub fn pack_image() -> Filters {
    only(crate::t::picker_filter_image(), &["png", "jpg", "jpeg", "gif", "webp", "tiff", "tif", "bmp", "ico"])
}

/// `select_custom_notification_sound`.
pub fn sound() -> Filters {
    only(crate::t::picker_filter_audio(), &["mp3", "wav", "flac", "ogg", "m4a"])
}
