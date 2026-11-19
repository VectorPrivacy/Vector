//! The app's language. A device setting, not an account one, so the login screen reads in it.

use std::path::{Path, PathBuf};

use crate::backend::device;

const KEY: &str = "ui_language";
const OFFERED_KEY: &str = "language_offered";

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
/// Where translating Vector starts, until the translation project has a home of its own.
pub const TRANSLATE_URL: &str = "https://github.com/VectorPrivacy/Vector/blob/gpui/crates/vector-i18n/TRANSLATING.md";

/// A count shown on its own (a badge, a tally) in the language's digits.
pub fn num(n: impl std::fmt::Display) -> gpui::SharedString {
    digits(n.to_string()).into()
}

/// Text built in code (a clock, `4:05`) with its digits in the language's own.
pub fn digits(s: String) -> String {
    vector_i18n::locale().native(s)
}

/// Fonts for what the family lacks, in the active language's regional Han forms. The system's
/// own fallback follows the system's language, so a Japanese app on an English desktop would
/// otherwise draw kanji in Chinese forms.
pub fn fallbacks() -> Option<gpui::FontFallbacks> {
    static CHAIN: std::sync::Mutex<(u64, Option<gpui::FontFallbacks>)> = std::sync::Mutex::new((u64::MAX, None));
    let generation = vector_i18n::generation();
    let mut chain = CHAIN.lock().unwrap_or_else(|e| e.into_inner());
    if chain.0 != generation {
        let fonts = han(&vector_i18n::language()).map(|h| gpui::FontFallbacks::from_fonts(han_fonts(h).iter().map(|f| f.to_string()).collect()));
        *chain = (generation, fonts);
    }
    chain.1.clone()
}

/// [`fallbacks`] on an element whose text doesn't inherit the window's: tooltips, drag ghosts.
pub trait LangFonts: gpui::Styled + Sized {
    fn lang_fonts(mut self) -> Self {
        self.text_style().font_fallbacks = fallbacks();
        self
    }
}

impl<T: gpui::Styled> LangFonts for T {}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Han {
    Ja,
    Ko,
    Sc,
    Tc,
    Hk,
}

fn han(tag: &str) -> Option<Han> {
    let mut parts = tag.split(['-', '_']);
    let lang = parts.next()?.to_ascii_lowercase();
    let rest: Vec<&str> = parts.collect();
    let has = |x: &str| rest.iter().any(|p| p.eq_ignore_ascii_case(x));
    Some(match lang.as_str() {
        "ja" => Han::Ja,
        "ko" => Han::Ko,
        "zh" | "yue" if has("Hans") => Han::Sc, // i18n-exempt: script subtag
        "zh" | "yue" if has("HK") || has("MO") => Han::Hk,
        "zh" if has("Hant") || has("TW") => Han::Tc, // i18n-exempt: script subtag
        "yue" => Han::Hk,
        "zh" => Han::Sc,
        _ => return None,
    })
}

#[cfg(target_os = "macos")]
fn han_fonts(h: Han) -> &'static [&'static str] {
    match h {
        Han::Ja => &["Hiragino Sans", "Hiragino Kaku Gothic ProN"], // i18n-exempt: font family names
        Han::Ko => &["Apple SD Gothic Neo"], // i18n-exempt: font family names
        Han::Sc => &["PingFang SC"], // i18n-exempt: font family names
        Han::Tc => &["PingFang TC"], // i18n-exempt: font family names
        Han::Hk => &["PingFang HK", "PingFang TC"], // i18n-exempt: font family names
    }
}

#[cfg(target_os = "windows")]
fn han_fonts(h: Han) -> &'static [&'static str] {
    match h {
        Han::Ja => &["Yu Gothic UI", "Meiryo UI", "Meiryo", "MS UI Gothic"], // i18n-exempt: font family names
        Han::Ko => &["Malgun Gothic"], // i18n-exempt: font family names
        Han::Sc => &["Microsoft YaHei UI", "Microsoft YaHei"], // i18n-exempt: font family names
        Han::Tc | Han::Hk => &["Microsoft JhengHei UI", "Microsoft JhengHei"], // i18n-exempt: font family names
    }
}

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
fn han_fonts(h: Han) -> &'static [&'static str] {
    match h {
        Han::Ja => &["Noto Sans CJK JP", "Noto Sans JP", "Source Han Sans JP", "IPAGothic", "IPAexGothic", "VL Gothic", "TakaoGothic"], // i18n-exempt: font family names
        Han::Ko => &["Noto Sans CJK KR", "Noto Sans KR", "Source Han Sans KR", "NanumGothic", "UnDotum"], // i18n-exempt: font family names
        Han::Sc => &["Noto Sans CJK SC", "Noto Sans SC", "Source Han Sans SC", "WenQuanYi Micro Hei", "WenQuanYi Zen Hei"], // i18n-exempt: font family names
        Han::Tc => &["Noto Sans CJK TC", "Noto Sans TC", "Source Han Sans TC", "AR PL UMing TW"], // i18n-exempt: font family names
        Han::Hk => &["Noto Sans CJK HK", "Noto Sans HK", "Source Han Sans HC", "Noto Sans CJK TC", "AR PL UMing HK"], // i18n-exempt: font family names
    }
}

/// Where downloaded packs live, beside the device settings.
pub fn packs_dir(data_dir: &Path) -> PathBuf {
    data_dir.join("languages")
}

pub fn pack_path(packs: &Path, tag: &str) -> PathBuf {
    packs.join(format!("{tag}.{PACK_EXT}"))
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
    for e in packs.as_ref().and_then(|d| std::fs::read_dir(d).ok()).into_iter().flatten().flatten() {
        if e.path().extension().is_some_and(|x| x == PACK_EXT)
            && let Ok(bytes) = std::fs::read(e.path())
            && let Ok(p) = vector_i18n::Pack::from_bytes(&bytes)
        {
            crate::backend::langpacks::note_installed(&p.tag, &bytes);
            vector_i18n::install(p);
        }
    }
    crate::backend::langpacks::init(packs);
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

/// A pack just downloaded: listed at once, and in use at once when it is the language on screen
/// or `switch` asks for it.
pub fn install(pack: vector_i18n::Pack, switch: bool, cx: &mut gpui::App) {
    let tag = pack.tag.clone();
    vector_i18n::install(pack);
    if switch || vector_i18n::language() == tag {
        set(&tag, cx);
    } else {
        cx.refresh_windows();
    }
}

/// The system's language as a tag (`de-DE`), from `de_DE.UTF-8` or `de-DE`.
pub fn system_tag() -> String {
    let raw = std::env::var("VECTOR_LOCALE").ok().map(|v| v.split(':').next().unwrap_or("").to_string()).or_else(sys_locale::get_locale).unwrap_or_default();
    raw.split(['.', '@']).next().unwrap_or("").replace('_', "-")
}

/// What a first launch offers: the system's language, when this build knows the repository
/// translates most of it and nothing has been chosen or offered yet.
pub fn offer() -> Option<&'static vector_i18n::Offered> {
    const MIN_OFFERED: u32 = 60;
    if device::get(KEY).is_some() || device::get(OFFERED_KEY).is_some() || std::env::var_os("VECTOR_LANG").is_some() {
        return None;
    }
    let system = system_tag();
    let primary = system.split('-').next().unwrap_or("");
    let fits = |o: &&vector_i18n::Offered| o.coverage >= MIN_OFFERED;
    let exact = vector_i18n::OFFERED.iter().filter(fits).find(|o| o.tag.eq_ignore_ascii_case(&system));
    exact.or_else(|| vector_i18n::OFFERED.iter().filter(fits).find(|o| !primary.is_empty() && o.tag.eq_ignore_ascii_case(primary)))
}

/// The offer was taken or turned down: it isn't made again.
pub fn offered(tag: &str) {
    device::set(OFFERED_KEY, tag);
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn han_forms_follow_the_tag() {
        assert_eq!(han("ja"), Some(Han::Ja));
        assert_eq!(han("ja-JP"), Some(Han::Ja));
        assert_eq!(han("ko_KR"), Some(Han::Ko));
        assert_eq!(han("zh"), Some(Han::Sc));
        assert_eq!(han("zh-Hans"), Some(Han::Sc));
        assert_eq!(han("zh-CN"), Some(Han::Sc));
        assert_eq!(han("zh-Hant"), Some(Han::Tc));
        assert_eq!(han("zh-TW"), Some(Han::Tc));
        assert_eq!(han("zh-Hant-HK"), Some(Han::Hk));
        assert_eq!(han("zh-HK"), Some(Han::Hk));
        assert_eq!(han("zh-Hans-HK"), Some(Han::Sc));
        assert_eq!(han("yue"), Some(Han::Hk));
        assert_eq!(han("en-GB"), None);
        assert_eq!(han("pt-BR"), None);
        assert_eq!(han(""), None);
    }
}
