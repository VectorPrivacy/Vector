//! The frozen normalizer bundle (§7.4) — bundle version 1.
//!
//! Every byte here is wire-frozen: two clients that normalize differently
//! convict different members from identical evidence, so each definition is
//! stated in Unicode properties rather than a stdlib call a non-Rust client
//! cannot match.
//!
//!  * `none`        — verbatim.
//!  * `fold`        — NFC → full case-fold → strip default-ignorables → NFC.
//!                    The trailing re-normalization is required: full folding
//!                    can denormalize (`ẞ`, `ﬁ`).
//!  * `skeleton`    — `fold`, then keep only Alphabetic scalars whose
//!                    `Numeric_Type` is None, plus the shortcode resolution
//!                    below. Digits are one line of attacker code to vary.
//!  * `confusables` — `fold` then the UTS-39 mapping (Phase 2; the table is a
//!                    bundle artifact, so it is declared and refused for now
//!                    rather than silently approximated).
//!
//! Shortcode resolution belongs to `skeleton` ALONE — `fold` resolving them
//! would change every `keyword` span in any message containing `:smile:`. Three
//! outcomes, identical for every consumer:
//!  * resolved (bundle list or the policy's `emoji_codes`) → contributes nothing
//!  * unresolved and shorter than `MIN_SKELETON_LEN` → contributes nothing
//!  * unresolved and long → contributes its inner text (this is what closes the
//!    colon-wrap evasion `:buycheapcoinsnow:`)

use super::document::Normalize;
use super::types::caps;
use std::collections::BTreeSet;
use unicode_normalization::UnicodeNormalization;

/// Shortcodes that render as an image for this community: the bundle's pinned
/// Unicode list plus the policy's declared `emoji_codes`. Community-scoped,
/// never viewer-scoped.
#[derive(Debug, Clone, Default)]
pub struct EmojiCodes(pub BTreeSet<String>);

impl EmojiCodes {
    pub fn from_policy<'a>(codes: impl IntoIterator<Item = &'a String>) -> Self {
        EmojiCodes(codes.into_iter().cloned().collect())
    }
    fn resolves(&self, name: &str) -> bool {
        self.0.contains(name)
    }
}

/// Default-ignorable code points the fold strips: ZWJ/ZWNJ, variation
/// selectors, bidi controls, soft hyphen, and the format class generally.
fn is_default_ignorable(c: char) -> bool {
    matches!(c as u32,
        0x00AD | 0x034F | 0x061C | 0x115F..=0x1160 | 0x17B4..=0x17B5 | 0x180B..=0x180F |
        0x200B..=0x200F | 0x202A..=0x202E | 0x2060..=0x206F | 0x3164 | 0xFE00..=0xFE0F |
        0xFEFF | 0xFFA0 | 0x1D173..=0x1D17A | 0xE0000..=0xE0FFF)
}

/// Alphabetic, excluding anything carrying a Numeric_Type. This is what Rust's
/// `is_alphanumeric() && !is_numeric()` computes; stating the property is what
/// lets a non-Rust client match it (it excludes `Nl`, e.g. `Ⅷ`).
fn is_skeleton_alpha(c: char) -> bool {
    c.is_alphanumeric() && !c.is_numeric()
}

/// NFC → full case-fold → strip default-ignorables → NFC.
pub fn fold(text: &str) -> String {
    let nfc: String = text.nfc().collect();
    let folded: String = nfc
        .chars()
        .filter(|c| !is_default_ignorable(*c))
        .flat_map(|c| c.to_lowercase())
        .collect();
    folded.nfc().collect()
}

/// A `:name:` token: `:` then 1+ of `[A-Za-z0-9_+-]` then `:` (the pinned
/// grammar). Returns the inner name and the byte index just past the closing
/// colon.
fn shortcode_at(s: &str, open: usize) -> Option<(&str, usize)> {
    let rest = &s[open + 1..];
    let close = rest.find(':')?;
    if close == 0 {
        return None;
    }
    let name = &rest[..close];
    if !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-' || c == '+') {
        return None;
    }
    Some((name, open + 1 + close + 1))
}

/// `skeleton` with community-scoped shortcode resolution.
pub fn skeleton(text: &str, codes: &EmojiCodes) -> String {
    // Resolution runs BEFORE folding: the token grammar is ASCII, so ordering
    // cannot change what resolves, and working on the raw text keeps the name
    // boundaries intact.
    let mut kept = String::with_capacity(text.len());
    let mut i = 0usize;
    let bytes = text.as_bytes();
    while i < text.len() {
        if bytes[i] == b':' {
            if let Some((name, next)) = shortcode_at(text, i) {
                // Resolved, or short enough to be a plausible emoji we do not
                // carry: contributes nothing. Otherwise it is prose in a
                // costume and its text is kept.
                if !codes.resolves(name) && name.chars().count() >= caps::MIN_SKELETON_LEN {
                    kept.push_str(name);
                }
                i = next;
                continue;
            }
        }
        let ch = text[i..].chars().next().expect("byte index is a char boundary");
        kept.push(ch);
        i += ch.len_utf8();
    }
    fold(&kept).chars().filter(|c| is_skeleton_alpha(*c)).collect()
}

/// Apply a declared normalizer. `Confusables` is declared in the bundle but its
/// table is a Phase-2 artifact: it falls back to `fold` and callers mark the
/// rule unevaluated rather than approximating a mapping two clients would
/// disagree on.
pub fn apply(text: &str, n: Normalize, codes: &EmojiCodes) -> String {
    match n {
        Normalize::None => text.to_string(),
        Normalize::Fold => fold(text),
        Normalize::Skeleton => skeleton(text, codes),
        Normalize::Confusables => fold(text),
    }
}

/// Is this normalizer implemented in bundle v1? `confusables` is not.
pub fn is_available(n: Normalize) -> bool {
    !matches!(n, Normalize::Confusables)
}

/// A token is a maximal run of code points that are Alphabetic or carry a
/// `Numeric_Type` other than None; every other code point separates. Used by
/// bare (token-anchored) keyword patterns — the Scunthorpe guard.
pub fn is_token_char(c: char) -> bool {
    c.is_alphanumeric()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fold_is_case_and_form_insensitive_and_strips_invisibles() {
        assert_eq!(fold("HELLO"), "hello");
        // Precomposed and decomposed é normalize together.
        assert_eq!(fold("café"), fold("cafe\u{0301}"));
        // Zero-width joiner smuggling collapses.
        assert_eq!(fold("s\u{200B}cam"), "scam");
        // Full folding can denormalize; the trailing NFC puts it back.
        assert_eq!(fold("ẞ"), fold("ß"));
    }

    #[test]
    fn skeleton_drops_the_cheap_variations() {
        let e = EmojiCodes::default();
        assert_eq!(skeleton("Hello, World!", &e), skeleton("hello world", &e));
        assert_eq!(skeleton("h3ll0 w0rld", &e), "hllwrld", "digits carry no weight");
        assert_ne!(skeleton("hello world", &e), skeleton("goodbye world", &e));
    }

    /// The colon-wrap evasion and the reaction false-positive, together — the
    /// pair that forced one resolution rule instead of a per-consumer split.
    #[test]
    fn shortcode_resolution_has_exactly_three_outcomes() {
        let known = EmojiCodes::from_policy([&"vector_logo".to_string()]);
        // Resolved: an image, contributes nothing.
        assert_eq!(skeleton(":vector_logo:", &known), "");
        assert_eq!(skeleton("nice :vector_logo: one", &known), skeleton("nice one", &known));
        // Unresolved and short: a plausible emoji from a set we do not carry.
        assert_eq!(skeleton(":fire:", &known), "");
        assert_eq!(skeleton(":+1:", &known), "");
        // Unresolved and long: prose in a costume. This is what the raid used.
        assert_eq!(skeleton(":buycheapcoinsnow:", &known), "buycheapcoinsnow");
        // An unlisted long custom emoji does share a key — bounded by cohort's
        // thinness bar, not by a special case here.
        assert_eq!(skeleton(":unlisted_pack_emoji:", &known), "unlistedpackemoji");
    }

    #[test]
    fn a_lone_or_unclosed_colon_is_left_alone() {
        let e = EmojiCodes::default();
        assert_eq!(skeleton("ratio 3:1 today", &e), skeleton("ratio 3 1 today", &e));
        assert_eq!(skeleton(":unclosed", &e), "unclosed");
        assert_eq!(skeleton("::", &e), "");
    }

    #[test]
    fn confusables_is_declared_but_not_yet_implemented() {
        assert!(!is_available(Normalize::Confusables));
        assert!(is_available(Normalize::Skeleton));
    }
}
