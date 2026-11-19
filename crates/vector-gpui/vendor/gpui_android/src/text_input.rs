//! The soft keyboard's side of a text field. Kotlin keeps a copy of the focused field that its
//! `InputConnection` edits, the way Android keyboards expect; after each edit it reports the
//! copy's new state, and the difference from the last state becomes one replacement in GPUI.
//! GPUI's state is then sent back, so the copy never drifts from the real field.

use crate::bridge;
use gpui::{Autocapitalize, PlatformInputHandler, TextInputAction, TextInputConfiguration};

/// A field in UTF-16 code units, as both sides index it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Field {
    pub text: Vec<u16>,
    pub selection: (usize, usize),
    pub composing: Option<(usize, usize)>,
}

impl Field {
    pub fn read(handler: &mut PlatformInputHandler) -> Option<Self> {
        let mut adjusted = None;
        let text: Vec<u16> = handler.text_for_range(0..usize::MAX, &mut adjusted)?.encode_utf16().collect();
        let len = text.len();
        let selection = handler
            .selected_text_range(false)
            .map(|s| (s.range.start.min(len), s.range.end.min(len)))
            .unwrap_or((len, len));
        let composing = handler
            .marked_text_range()
            .filter(|r| r.start < r.end && r.end <= len)
            .map(|r| (r.start, r.end));
        Some(Self { text, selection, composing })
    }

    pub fn from_keyboard(text: &str, selection: (i32, i32), composing: (i32, i32)) -> Self {
        let text: Vec<u16> = text.encode_utf16().collect();
        let clamp = |v: i32| (v.max(0) as usize).min(text.len());
        let (a, b) = (clamp(selection.0), clamp(selection.1));
        let composing = (composing.0 >= 0 && composing.1 > composing.0)
            .then(|| (clamp(composing.0), clamp(composing.1)))
            .filter(|(s, e)| s < e);
        Self { selection: (a.min(b), a.max(b)), composing, text }
    }

    pub fn send(&self) {
        let (cs, ce) = self.composing.map_or((-1, -1), |(s, e)| (s as i32, e as i32));
        bridge::set_text_state(
            &String::from_utf16_lossy(&self.text),
            (self.selection.0 as i32, self.selection.1 as i32),
            (cs, ce),
        );
    }
}

/// What changed from `old` to `new`: the replaced range in `old` and its replacement's range
/// in `new`.
pub(crate) fn changed(old: &[u16], new: &[u16]) -> (std::ops::Range<usize>, std::ops::Range<usize>) {
    let prefix = old.iter().zip(new).take_while(|(a, b)| a == b).count();
    let room = old.len().min(new.len()) - prefix;
    let suffix = old
        .iter()
        .rev()
        .zip(new.iter().rev())
        .take(room)
        .take_while(|(a, b)| a == b)
        .count();
    (prefix..old.len() - suffix, prefix..new.len() - suffix)
}

/// Applies the keyboard's edit from `old` to `new` to GPUI's field.
pub(crate) fn apply(handler: &mut PlatformInputHandler, old: &Field, new: &Field) {
    let (old_range, new_range) = changed(&old.text, &new.text);
    if let Some((cs, ce)) = new.composing {
        // The marked text is the replacement, so the change widens to the composing region.
        let start = new_range.start.min(cs);
        let end = new_range.end.max(ce);
        // Past `end` both texts agree, so `end` sits at `end - growth` in the old one.
        let growth = new.text.len() as isize - old.text.len() as isize;
        let old_end = (end as isize - growth).clamp(start as isize, old.text.len() as isize) as usize;
        let text = String::from_utf16_lossy(&new.text[start..end]);
        let selected = new.selection.0.saturating_sub(start)..new.selection.1.saturating_sub(start);
        handler.replace_and_mark_text_in_range(Some(start..old_end), &text, Some(selected));
        return;
    }
    if old_range.is_empty() && new_range.is_empty() {
        if old.composing.is_some() {
            handler.unmark_text();
        }
        return;
    }
    let text = String::from_utf16_lossy(&new.text[new_range]);
    handler.replace_text_in_range(Some(old_range), &text);
}

/// The configuration in one int for the bridge: bit 0 autocorrect, bit 1 suggestions, bits 2-3
/// capitalisation, bits 4-7 the action key.
pub(crate) fn options(config: &TextInputConfiguration) -> i32 {
    let caps = match config.autocapitalize {
        Autocapitalize::None => 0,
        Autocapitalize::Words => 1,
        Autocapitalize::Sentences => 2,
        Autocapitalize::Characters => 3,
    };
    let action = match config.input_action {
        TextInputAction::Unspecified => 0,
        TextInputAction::Enter => 1,
        TextInputAction::Done => 2,
        TextInputAction::Go => 3,
        TextInputAction::Next => 4,
        TextInputAction::Previous => 5,
        TextInputAction::Search => 6,
        TextInputAction::Send => 7,
    };
    config.autocorrect as i32 | (config.suggestions as i32) << 1 | caps << 2 | action << 4
}

#[cfg(test)]
mod tests {
    use super::*;

    fn u(s: &str) -> Vec<u16> {
        s.encode_utf16().collect()
    }

    #[test]
    fn a_typed_letter_is_an_insertion() {
        assert_eq!(changed(&u("helo"), &u("hello")), (3..3, 3..4));
    }

    #[test]
    fn a_deletion_replaces_with_nothing() {
        assert_eq!(changed(&u("hello"), &u("helo")), (3..4, 3..3));
    }

    #[test]
    fn repeated_letters_do_not_overlap_prefix_and_suffix() {
        assert_eq!(changed(&u("aa"), &u("aaa")), (2..2, 2..3));
        assert_eq!(changed(&u("aaa"), &u("aa")), (2..3, 2..2));
    }

    #[test]
    fn an_unchanged_field_changes_nothing() {
        assert_eq!(changed(&u("same"), &u("same")), (4..4, 4..4));
    }
}
