//! The app's language. A device setting, not an account one, so the login screen reads in it.

use crate::backend::device;

const KEY: &str = "ui_language";

/// The saved language, after any packs a translator points the app at with `VECTOR_LANG_DIR`
/// (a folder of language folders, as in `crates/vector-i18n/locales`). `VECTOR_LANG` overrides.
pub fn init() {
    if let Some(root) = std::env::var_os("VECTOR_LANG_DIR") {
        for e in std::fs::read_dir(&root).into_iter().flatten().flatten() {
            let tag = e.file_name().to_string_lossy().into_owned();
            if e.path().is_dir() && tag != vector_i18n::BUILT_IN && let Ok(p) = vector_i18n::Pack::from_dir(&tag, &e.path()) {
                vector_i18n::install(p);
            }
        }
    }
    if let Some(tag) = std::env::var("VECTOR_LANG").ok().or_else(|| device::get(KEY)) {
        vector_i18n::set_language(&tag);
    }
}

/// Switches every window to `tag` at once and keeps it for next launch.
pub fn set(tag: &str, cx: &mut gpui::App) {
    vector_i18n::set_language(tag);
    device::set(KEY, vector_i18n::language());
    cx.refresh_windows();
}
