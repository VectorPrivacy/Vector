//! Spelling, as the system checks it: the misspelled words in a text, a word's guesses and its
//! autocorrection. The webview gave the desktop client this for free; here each platform's own
//! checker answers through [`Backend`], and a platform without one checks nothing. Ranges are
//! byte ranges into the text asked about.

use std::ops::Range;

pub trait Backend {
    /// The misspelled words, in order.
    fn misspelled(&self, text: &str) -> Vec<Range<usize>>;
    /// Replacements for the word at `range`, best first.
    fn guesses(&self, text: &str, range: Range<usize>) -> Vec<String>;
    /// What autocorrect would put in place of the word at `range`, if anything.
    fn correction(&self, text: &str, range: Range<usize>) -> Option<String>;
    /// Never flag this word again (Learn Spelling).
    fn learn(&self, word: &str);
    /// Stop flagging this word until the app quits (Ignore Spelling).
    fn ignore(&self, word: &str);
}

/// This platform's checker. Asked on the main thread only.
pub fn backend() -> &'static dyn Backend {
    #[cfg(target_os = "macos")]
    {
        thread_local!(static MAC: &'static mac::Mac = Box::leak(Box::new(mac::Mac::new())));
        MAC.with(|m| *m as &'static dyn Backend)
    }
    #[cfg(windows)]
    {
        thread_local!(static WIN: &'static dyn Backend = match win::Win::new() {
            Some(w) => Box::leak(Box::new(w)),
            None => &NoSpelling,
        });
        WIN.with(|w| *w)
    }
    #[cfg(not(any(target_os = "macos", windows)))]
    {
        &NoSpelling
    }
}

/// A platform without a checker yet: nothing is ever misspelled.
#[allow(dead_code)]
pub struct NoSpelling;

impl Backend for NoSpelling {
    fn misspelled(&self, _: &str) -> Vec<Range<usize>> {
        Vec::new()
    }
    fn guesses(&self, _: &str, _: Range<usize>) -> Vec<String> {
        Vec::new()
    }
    fn correction(&self, _: &str, _: Range<usize>) -> Option<String> {
        None
    }
    fn learn(&self, _: &str) {}
    fn ignore(&self, _: &str) {}
}

/// Byte offset → UTF-16 offset, as `NSString` counts.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
fn utf16_at(text: &str, byte: usize) -> usize {
    text[..byte.min(text.len())].encode_utf16().count()
}

/// UTF-16 offset → byte offset, at a char boundary.
#[cfg_attr(not(any(target_os = "macos", windows)), allow(dead_code))]
fn byte_at(text: &str, unit: usize) -> usize {
    let mut n = 0;
    for (i, c) in text.char_indices() {
        if n >= unit {
            return i;
        }
        n += c.len_utf16();
    }
    text.len()
}

#[cfg(target_os = "macos")]
mod mac {
    use super::{Backend, Range, byte_at, utf16_at};
    use objc2::rc::Retained;
    use objc2_app_kit::NSSpellChecker;
    use objc2_foundation::{NSInteger, NSNotFound, NSRange, NSString};

    pub struct Mac {
        checker: Retained<NSSpellChecker>,
        /// One document for the whole app: Ignore applies everywhere it types.
        tag: NSInteger,
    }

    impl Mac {
        pub fn new() -> Self {
            Self { checker: NSSpellChecker::sharedSpellChecker(), tag: NSSpellChecker::uniqueSpellDocumentTag() }
        }

        fn range(text: &str, r: &Range<usize>) -> NSRange {
            let start = utf16_at(text, r.start);
            NSRange::new(start, utf16_at(text, r.end) - start)
        }
    }

    impl Backend for Mac {
        fn misspelled(&self, text: &str) -> Vec<Range<usize>> {
            let ns = NSString::from_str(text);
            let total = ns.length();
            let mut out = Vec::new();
            let mut at = 0usize;
            while at < total {
                // SAFETY: a null word count is allowed.
                let r = unsafe {
                    self.checker.checkSpellingOfString_startingAt_language_wrap_inSpellDocumentWithTag_wordCount(
                        &ns,
                        at as NSInteger,
                        None,
                        false,
                        self.tag,
                        std::ptr::null_mut(),
                    )
                };
                if r.location == NSNotFound as usize || r.length == 0 || r.location < at {
                    break;
                }
                out.push(byte_at(text, r.location)..byte_at(text, r.location + r.length));
                at = r.location + r.length;
            }
            out
        }

        fn guesses(&self, text: &str, range: Range<usize>) -> Vec<String> {
            let ns = NSString::from_str(text);
            self.checker
                .guessesForWordRange_inString_language_inSpellDocumentWithTag(Self::range(text, &range), &ns, None, self.tag)
                .map(|a| a.iter().map(|s| s.to_string()).collect())
                .unwrap_or_default()
        }

        fn correction(&self, text: &str, range: Range<usize>) -> Option<String> {
            let ns = NSString::from_str(text);
            let lang = self.checker.language();
            self.checker
                .correctionForWordRange_inString_language_inSpellDocumentWithTag(Self::range(text, &range), &ns, &lang, self.tag)
                .map(|s| s.to_string())
        }

        fn learn(&self, word: &str) {
            self.checker.learnWord(&NSString::from_str(word));
        }

        fn ignore(&self, word: &str) {
            self.checker.ignoreWord_inSpellDocumentWithTag(&NSString::from_str(word), self.tag);
        }
    }
}

#[cfg(windows)]
mod win {
    use super::{Backend, Range, byte_at};
    use windows::Win32::Globalization::{
        CORRECTIVE_ACTION_REPLACE, GetUserDefaultLocaleName, ISpellChecker, ISpellCheckerFactory, ISpellingError,
        SpellCheckerFactory,
    };
    use windows::Win32::System::Com::{CLSCTX_INPROC_SERVER, CoCreateInstance, CoTaskMemFree};
    use windows::core::{HSTRING, PCWSTR, PWSTR};

    /// The Windows 8+ spell checker, in the user's language (English where it has none).
    pub struct Win {
        checker: ISpellChecker,
    }

    /// A string the checker allocated, read and freed.
    fn take(p: PWSTR) -> Option<String> {
        if p.is_null() {
            return None;
        }
        // SAFETY: the checker hands back a NUL-terminated string from CoTaskMemAlloc.
        let s = unsafe { p.to_string().ok() };
        unsafe { CoTaskMemFree(Some(p.0 as *const _)) };
        s
    }

    impl Win {
        pub fn new() -> Option<Self> {
            // SAFETY: COM is initialised on the main thread by the platform before any window.
            unsafe {
                let factory: ISpellCheckerFactory = CoCreateInstance(&SpellCheckerFactory, None, CLSCTX_INPROC_SERVER).ok()?;
                let mut buf = [0u16; 85];
                let n = GetUserDefaultLocaleName(&mut buf);
                let mine = (n > 1).then(|| String::from_utf16_lossy(&buf[..n as usize - 1]));
                // i18n-exempt: locale tag
                for lang in mine.iter().map(String::as_str).chain(["en-US"]) {
                    let tag = HSTRING::from(lang);
                    if factory.IsSupported(PCWSTR(tag.as_ptr())).is_ok_and(|b| b.as_bool()) {
                        return factory.CreateSpellChecker(PCWSTR(tag.as_ptr())).ok().map(|checker| Self { checker });
                    }
                }
                None
            }
        }

        fn errors(&self, text: &str) -> Vec<(Range<usize>, Option<String>)> {
            let wide = HSTRING::from(text);
            let mut out = Vec::new();
            // SAFETY: plain COM calls on a live checker with a NUL-terminated string.
            unsafe {
                let Ok(list) = self.checker.Check(PCWSTR(wide.as_ptr())) else { return out };
                loop {
                    let mut e: Option<ISpellingError> = None;
                    if list.Next(&mut e).is_err() {
                        break;
                    }
                    let Some(e) = e else { break };
                    let (Ok(at), Ok(len)) = (e.StartIndex(), e.Length()) else { continue };
                    let fix = (e.CorrectiveAction().ok() == Some(CORRECTIVE_ACTION_REPLACE))
                        .then(|| e.Replacement().ok().and_then(take))
                        .flatten();
                    let (at, len) = (at as usize, len as usize);
                    out.push((byte_at(text, at)..byte_at(text, at + len), fix));
                }
            }
            out
        }
    }

    impl Backend for Win {
        fn misspelled(&self, text: &str) -> Vec<Range<usize>> {
            // A Latin-script checker has no say over kanji, kana or Hangul.
            let foreign = |r: &Range<usize>| text.get(r.clone()).is_some_and(|w| w.chars().any(|c| crate::select::word_class(c) >= 3 || ('\u{ac00}'..='\u{d7af}').contains(&c)));
            self.errors(text).into_iter().map(|(r, _)| r).filter(|r| !foreign(r)).collect()
        }

        fn guesses(&self, text: &str, range: Range<usize>) -> Vec<String> {
            let Some(word) = text.get(range) else { return Vec::new() };
            let wide = HSTRING::from(word);
            let mut out = Vec::new();
            // SAFETY: plain COM calls; each string handed back is freed by `take`.
            unsafe {
                let Ok(list) = self.checker.Suggest(PCWSTR(wide.as_ptr())) else { return out };
                loop {
                    let mut one = [PWSTR::null()];
                    let mut got = 0u32;
                    if list.Next(&mut one, Some(&mut got)).is_err() || got == 0 {
                        break;
                    }
                    if let Some(s) = take(one[0]) {
                        out.push(s);
                    }
                }
            }
            out
        }

        fn correction(&self, text: &str, range: Range<usize>) -> Option<String> {
            let word = text.get(range)?;
            self.errors(word).into_iter().find_map(|(r, fix)| (r == (0..word.len())).then_some(fix).flatten())
        }

        fn learn(&self, word: &str) {
            let wide = HSTRING::from(word);
            // SAFETY: a plain COM call with a NUL-terminated string.
            let _ = unsafe { self.checker.Add(PCWSTR(wide.as_ptr())) };
        }

        fn ignore(&self, word: &str) {
            let wide = HSTRING::from(word);
            // SAFETY: a plain COM call with a NUL-terminated string.
            let _ = unsafe { self.checker.Ignore(PCWSTR(wide.as_ptr())) };
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn offsets_survive_wide_characters() {
        let t = "héllo 👋 wrld";
        let b = t.find("wrld").unwrap();
        assert_eq!(byte_at(t, utf16_at(t, b)), b);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn the_system_checker_finds_and_fixes() {
        let s = backend();
        let t = "Thiss line 👋 has erorrs";
        let words: Vec<&str> = s.misspelled(t).into_iter().map(|r| &t[r]).collect();
        // A headless runner may have no dictionary loaded, or guesses without a checking language.
        if words.is_empty() {
            eprintln!("no system dictionary; skipped");
            return;
        }
        assert_eq!(words, ["Thiss", "erorrs"]);
        let r = t.find("erorrs").unwrap();
        let g = s.guesses(t, r..r + 6);
        assert!(g.iter().any(|g| g == "errors"), "{g:?}");
    }
}
