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
    #[cfg(not(target_os = "macos"))]
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
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
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
        assert_eq!(words, ["Thiss", "erorrs"]);
        let r = t.find("erorrs").unwrap();
        let g = s.guesses(t, r..r + 6);
        assert!(g.iter().any(|g| g == "errors"), "{g:?}");
    }
}
