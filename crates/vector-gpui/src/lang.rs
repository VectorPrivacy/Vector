//! The app's language. A device setting, not an account one, so the login screen reads in it.

use std::path::{Path, PathBuf};

use crate::backend::device;

const KEY: &str = "ui_language";

/// Icons that point along the reading direction, flipped when it runs right to left. Media
/// controls keep their direction: time runs left to right in every language.
const MIRRORED_ICONS: &[&str] = &[
    "icons/chevron-left.svg",
    "icons/chevron-right.svg",
    "icons/chevron-double-left.svg",
    "icons/reply.svg",
    "icons/send.svg",
    "icons/share.svg",
    "ui/notice-go.svg",
];
const PACK_EXT: &str = "vpack";

/// Where downloaded packs live, beside the device settings.
pub fn packs_dir(data_dir: &Path) -> PathBuf {
    data_dir.join("languages")
}

/// The saved language, after the packs downloaded to `packs` and any a translator points the
/// app at with `VECTOR_LANG_DIR` (a folder of language folders, as in
/// `crates/vector-i18n/locales`). `VECTOR_LANG` overrides. The system's locale and clock come
/// first: English's date order and the clock follow them.
pub fn init(packs: Option<PathBuf>) {
    // VECTOR_LOCALE stands in for the system's, as `en-US` or `en-GB:24`.
    let (locale, hour12) = match std::env::var("VECTOR_LOCALE") {
        Ok(v) => match v.split_once(':') {
            Some((tag, clock)) => (tag.to_string(), Some(clock == "12")),
            None => (v, None),
        },
        Err(_) => (sys_locale::get_locale().unwrap_or_default(), system_hour12()),
    };
    vector_i18n::set_system_locale(&locale, hour12);
    gpui::set_mirrored_icons(MIRRORED_ICONS);
    for e in packs.and_then(|d| std::fs::read_dir(d).ok()).into_iter().flatten().flatten() {
        if e.path().extension().is_some_and(|x| x == PACK_EXT)
            && let Some(p) = std::fs::read(e.path()).ok().and_then(|b| vector_i18n::Pack::from_bytes(&b).ok())
        {
            vector_i18n::install(p);
        }
    }
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
    gpui::set_layout_rtl(vector_i18n::is_rtl());
}

/// Switches every window to `tag` at once and keeps it for next launch.
pub fn set(tag: &str, cx: &mut gpui::App) {
    vector_i18n::set_language(tag);
    gpui::set_layout_rtl(vector_i18n::is_rtl());
    device::set(KEY, vector_i18n::language());
    cx.refresh_windows();
}

/// The clock the system asks for, where it lets people choose one apart from their region.
#[cfg(target_os = "macos")]
fn system_hour12() -> Option<bool> {
    use objc2_foundation::{NSDateFormatter, NSLocale, NSString};
    // "j" is the locale's preferred hour, with the 24-hour switch applied: "h a" or "HH".
    let f = NSDateFormatter::dateFormatFromTemplate_options_locale(&NSString::from_str("j"), 0, Some(&NSLocale::currentLocale()))?;
    Some(!f.to_string().contains(['H', 'k']))
}

#[cfg(windows)]
fn system_hour12() -> Option<bool> {
    use windows::Win32::Globalization::{GetLocaleInfoEx, LOCALE_STIMEFORMAT};
    let mut buf = [0u16; 80];
    // SAFETY: a null name is the user's default locale; the buffer's length is passed with it.
    let n = unsafe { GetLocaleInfoEx(windows::core::PCWSTR::null(), LOCALE_STIMEFORMAT, Some(&mut buf)) };
    let format = String::from_utf16_lossy(buf.get(..(n as usize).saturating_sub(1))?);
    (n > 0).then(|| !format.contains('H'))
}

#[cfg(not(any(target_os = "macos", windows)))]
fn system_hour12() -> Option<bool> {
    None
}
