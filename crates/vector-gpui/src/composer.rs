//! The message composer's text field: GPUI's own input (zed `crates/gpui/examples/input.rs`,
//! Apache-2.0), grown to wrap and take newlines, restyled as `#chat-input.rich-composer` and
//! wired to send on Enter.

use std::ops::Range;

use gpui::{
    App, Bounds, ClipboardItem, Context, CursorStyle, ElementId, ElementInputHandler, Entity,
    EntityInputHandler, EventEmitter, FocusHandle, Focusable, GlobalElementId, KeyBinding,
    LayoutId, MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, PaintQuad, Pixels, Point,
    ScrollHandle, SharedString, Style, TextRun, UTF16Selection, UnderlineStyle, Window,
    WrappedLine, actions, div, fill, point, prelude::*, px, relative, size,
};
use unicode_segmentation::*;

use gpui::Hsla;

use crate::theme::*;

actions!(
    text_input,
    [
        Backspace,
        Delete,
        Left,
        Right,
        SelectLeft,
        SelectRight,
        SelectAll,
        Home,
        End,
        ShowCharacterPalette,
        Paste,
        Cut,
        Copy,
        Send,
        Cancel,
        Newline,
        Up,
        Down,
        SelectUp,
        SelectDown,
        Tab,
        Undo,
        Redo,
        WordLeft,
        WordRight,
        SelectWordLeft,
        SelectWordRight,
        DeleteWordLeft,
        DeleteWordRight,
        SelectHome,
        SelectEnd,
        Bold,
        Italic,
        Code,
        Strike,
        SendNow,
    ]
);

pub struct TextInput {
    focus_handle: FocusHandle,
    content: SharedString,
    placeholder: SharedString,
    selected_range: Range<usize>,
    selection_reversed: bool,
    marked_range: Option<Range<usize>>,
    last_layout: Option<Layout>,
    last_bounds: Option<Bounds<Pixels>>,
    /// Wraps and takes newlines, scrolling past this height.
    multiline: Option<Pixels>,
    center: bool,
    right: bool,
    scroll: ScrollHandle,
    /// How far a one-line field's text is scrolled left to keep the caret in view.
    hscroll: std::cell::Cell<Pixels>,
    /// The caret moved: scroll it into view on the next paint.
    reveal: bool,
    is_selecting: bool,
    look: Look,
    /// Enter keeps the text (a login field's caller reads and clears it).
    clear_on_submit: bool,
    submit_empty: bool,
    /// Draws stock emoji as Twemoji art, as the rich composer does.
    twemoji: bool,
    /// Typing and pasting stop at this many characters.
    max_chars: Option<usize>,
    /// The emoji drawn as art, for the content they were found in.
    subs: std::cell::RefCell<(SharedString, std::rc::Rc<[Sub]>)>,
    /// An autocomplete is open: its keys go to it as `Nav`.
    nav: bool,
    edges: bool,
    colors: Option<bool>,
    chip: bool,
    /// Markdown and tags styled in place, with the names a tag may carry.
    decorated: Option<Vec<String>>,
    /// The name a pasted `@npub1…` shows as.
    npub_label: Option<std::rc::Rc<dyn Fn(&str) -> String>>,
    /// Pack emoji by shortcode, drawn as their art.
    customs: std::rc::Rc<std::collections::HashMap<String, String>>,
    /// The decorated spans drawn on a chip, in display offsets: (range, fill, radius, side pad).
    chips: std::cell::RefCell<Vec<(Range<usize>, Hsla, f32, f32)>>,
    /// Earlier states for Undo, and what Undo took back for Redo.
    undo: Vec<(SharedString, Range<usize>)>,
    redo: Vec<(SharedString, Range<usize>)>,
    /// The last edit was typing, and when: a run of it undoes as one step.
    typed: Option<std::time::Instant>,
}

/// A key an open autocomplete takes from the field.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Nav {
    Up,
    Down,
    Pick,
    Close,
}

/// Content drawn as something else: an emoji as art (`src`) over a placeholder, or a pasted
/// `@npub1…` as the person's name, which the caret steps over whole.
struct Sub {
    content: Range<usize>,
    display: Range<usize>,
    src: Option<gpui::ImageSource>,
    label: Option<SharedString>,
}

/// Each `@npub1…` mention outside code, as `cmpTokenize` finds them.
fn npub_mentions(text: &str) -> Vec<Range<usize>> {
    const BECH: &[u8] = b"023456789acdefghjklmnpqrstuvwxyz";
    let code: Vec<Range<usize>> = crate::decorate::tokenize(text, &[])
        .into_iter()
        .filter(|(_, d)| matches!(d, crate::decorate::Deco::Code | crate::decorate::Deco::CodeMark))
        .map(|(r, _)| r)
        .collect();
    let b = text.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while let Some(at) = text[i..].find("@npub1").map(|o| i + o) {
        let end = at + 6 + 58;
        let ok = b.len() >= end && b[at + 6..end].iter().all(|c| BECH.contains(c));
        if ok && !code.iter().any(|r| r.start <= at && at < r.end) {
            out.push(at..end);
            i = end;
        } else {
            i = at + 1;
        }
    }
    out
}

/// What an emoji's art occupies in the drawn text: 1.25em (`.cmp-twemoji`), an em space and a quarter-em.
const EMOJI_SLOT: &str = "\u{2003}\u{2005}";

/// How a field draws: the composer's, or a login field's.
#[derive(Clone)]
pub struct Look {
    pub family: &'static str,
    pub weight: gpui::FontWeight,
    pub size: Pixels,
    pub line_height: Pixels,
    pub color: Hsla,
    pub placeholder: Hsla,
    /// Shows a bullet per character, as a password field does.
    pub masked: bool,
    /// The size bullets take once something is typed.
    pub masked_size: Option<Pixels>,
}

impl Default for Look {
    fn default() -> Self {
        Self {
            family: RUBIK,
            weight: gpui::FontWeight::NORMAL,
            size: px(15.),
            line_height: px(21.),
            color: white_a(0.85),
            placeholder: white_a(0.38),
            masked: false,
            masked_size: None,
        }
    }
}

const BULLET: &str = "\u{2022}";

/// The drawn text as shaped: each hard line with its byte start and top.
struct Layout {
    text: SharedString,
    lines: Vec<(usize, Pixels, WrappedLine)>,
    line_height: Pixels,
    /// Centred within this width, each visual row on its own.
    align: Option<Pixels>,
    right: bool,
}

impl Layout {
    fn shape(
        text: SharedString,
        runs: &[TextRun],
        font_size: Pixels,
        wrap: Option<Pixels>,
        line_height: Pixels,
        window: &Window,
    ) -> Self {
        let shaped = window
            .text_system()
            .shape_text(text.clone(), font_size, runs, wrap, None)
            .unwrap_or_default();
        let mut lines = Vec::with_capacity(shaped.len());
        let (mut start, mut y) = (0, px(0.));
        for line in shaped {
            let (len, h) = (line.len(), line.size(line_height).height);
            lines.push((start, y, line));
            start += len + 1;
            y += h;
        }
        Self {
            text,
            lines,
            line_height,
            align: None,
            right: false,
        }
    }

    /// How far a centred row sits right of the left edge.
    fn shift(&self, line: &WrappedLine, row: usize) -> Pixels {
        let Some(aw) = self.align else { return px(0.) };
        let x = |b: &gpui::WrapBoundary| {
            line.unwrapped_layout.runs[b.run_ix].glyphs[b.glyph_ix]
                .position
                .x
        };
        let start = if row == 0 {
            px(0.)
        } else {
            line.wrap_boundaries.get(row - 1).map(x).unwrap_or(px(0.))
        };
        let end = line
            .wrap_boundaries
            .get(row)
            .map(x)
            .unwrap_or(line.unwrapped_layout.width);
        let room = (aw - (end - start)).max(px(0.));
        if self.right { room } else { room / 2. }
    }

    fn height(&self) -> Pixels {
        self.lines
            .last()
            .map(|(_, y, l)| *y + l.size(self.line_height).height)
            .unwrap_or(self.line_height)
            .max(self.line_height)
    }

    fn line_at(&self, index: usize) -> Option<&(usize, Pixels, WrappedLine)> {
        self.lines
            .iter()
            .rev()
            .find(|(start, _, _)| *start <= index)
    }

    /// Where `index` sits, from the text's top left.
    fn position(&self, index: usize) -> Point<Pixels> {
        let Some((start, y, line)) = self.line_at(index) else {
            return point(px(0.), px(0.));
        };
        let local = index.saturating_sub(*start).min(line.len());
        let p = line
            .position_for_index(local, self.line_height)
            .unwrap_or(point(line.width(), px(0.)));
        let row = (p.y / self.line_height).floor() as usize;
        point(p.x + self.shift(line, row), p.y + *y)
    }

    fn index_at(&self, p: Point<Pixels>) -> usize {
        let Some((start, y, line)) = self
            .lines
            .iter()
            .rev()
            .find(|(_, y, _)| *y <= p.y)
            .or(self.lines.first())
        else {
            return 0;
        };
        let h = line.size(self.line_height).height;
        let ly = (p.y - *y).clamp(px(0.), (h - px(1.)).max(px(0.)));
        let shift = self.shift(line, (ly / self.line_height).floor() as usize);
        let local = point((p.x - shift).max(px(0.)), ly);
        let i = match line.closest_index_for_position(local, self.line_height) {
            Ok(i) | Err(i) => i,
        };
        start + i.min(line.len())
    }
}

impl TextInput {
    fn left(&mut self, _: &Left, window: &mut Window, cx: &mut Context<Self>) {
        if self.edges && self.selected_range == (0..0) {
            return cx.emit(Edge::Start);
        }
        if self.selected_range.is_empty() {
            self.move_to(self.previous_boundary(self.cursor_offset()), window, cx);
        } else {
            self.move_to(self.selected_range.start, window, cx)
        }
    }

    fn right(&mut self, _: &Right, window: &mut Window, cx: &mut Context<Self>) {
        let end = self.content.len();
        if self.edges && self.selected_range == (end..end) {
            return cx.emit(Edge::End);
        }
        if self.selected_range.is_empty() {
            self.move_to(self.next_boundary(self.selected_range.end), window, cx);
        } else {
            self.move_to(self.selected_range.end, window, cx)
        }
    }

    fn select_left(&mut self, _: &SelectLeft, window: &mut Window, cx: &mut Context<Self>) {
        self.select_to(self.previous_boundary(self.cursor_offset()), window, cx);
    }

    fn select_right(&mut self, _: &SelectRight, window: &mut Window, cx: &mut Context<Self>) {
        self.select_to(self.next_boundary(self.cursor_offset()), window, cx);
    }

    fn select_all(&mut self, _: &SelectAll, window: &mut Window, cx: &mut Context<Self>) {
        self.move_to(0, window, cx);
        self.select_to(self.content.len(), window, cx)
    }

    fn home(&mut self, _: &Home, window: &mut Window, cx: &mut Context<Self>) {
        self.move_to(0, window, cx);
    }

    fn end(&mut self, _: &End, window: &mut Window, cx: &mut Context<Self>) {
        self.move_to(self.content.len(), window, cx);
    }

    fn backspace(&mut self, _: &Backspace, window: &mut Window, cx: &mut Context<Self>) {
        if self.edges && self.content.is_empty() {
            return cx.emit(Edge::Empty);
        }
        if self.selected_range.is_empty() {
            let prev = self.previous_boundary(self.cursor_offset());
            if self.cursor_offset() == prev {
                window.play_system_bell();
                return;
            }
            self.select_to(prev, window, cx)
        }
        self.replace_text_in_range(None, "", window, cx)
    }

    fn delete(&mut self, _: &Delete, window: &mut Window, cx: &mut Context<Self>) {
        if self.edges && self.content.is_empty() {
            return cx.emit(Edge::Empty);
        }
        if self.selected_range.is_empty() {
            let next = self.next_boundary(self.cursor_offset());
            if self.cursor_offset() == next {
                window.play_system_bell();
                return;
            }
            self.select_to(next, window, cx)
        }
        self.replace_text_in_range(None, "", window, cx)
    }

    fn on_mouse_down(
        &mut self,
        event: &MouseDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.is_selecting = true;
        window.request_virtual_keyboard();

        if event.modifiers.shift {
            self.select_to(self.index_for_mouse_position(event.position), window, cx);
        } else {
            self.move_to(self.index_for_mouse_position(event.position), window, cx)
        }
    }

    fn on_mouse_up(&mut self, _: &MouseUpEvent, _window: &mut Window, _: &mut Context<Self>) {
        self.is_selecting = false;
    }

    fn on_mouse_move(
        &mut self,
        event: &MouseMoveEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.is_selecting {
            self.select_to(self.index_for_mouse_position(event.position), window, cx);
        }
    }

    fn show_character_palette(
        &mut self,
        _: &ShowCharacterPalette,
        window: &mut Window,
        _: &mut Context<Self>,
    ) {
        window.show_character_palette();
    }

    fn paste(&mut self, _: &Paste, window: &mut Window, cx: &mut Context<Self>) {
        let files = crate::fileclip::read(cx);
        if !files.is_empty() {
            cx.emit(Pasted::Paths(files));
            return;
        }
        let item = cx.read_from_clipboard();
        if item.as_ref().and_then(|i| i.text()).is_none() {
            for entry in item.iter().flat_map(|i| i.entries()) {
                match entry {
                    gpui::ClipboardEntry::Image(img) => {
                        cx.emit(Pasted::Image {
                            bytes: img.bytes.clone(),
                            ext: img.format.extension(),
                        });
                        return;
                    }
                    gpui::ClipboardEntry::ExternalPaths(paths) => {
                        if !paths.paths().is_empty() {
                            cx.emit(Pasted::Paths(paths.paths().to_vec()));
                            return;
                        }
                    }
                    _ => {}
                }
            }
        }
        if let Some(text) = item.and_then(|item| item.text()) {
            let text = if self.multiline.is_some() {
                let text = text.replace("\r\n", "\n");
                // Styled copies keep their styling as markdown, so what shows here is what sends.
                crate::paste_md::clipboard_html()
                    .and_then(|html| crate::paste_md::html_to_markdown(&html, &text))
                    .unwrap_or(text)
            } else {
                text.replace('\n', " ")
            };
            self.replace_text_in_range(None, &text, window, cx);
        }
    }

    fn copy(&mut self, _: &Copy, _: &mut Window, cx: &mut Context<Self>) {
        if !self.selected_range.is_empty() {
            cx.write_to_clipboard(ClipboardItem::new_string(
                self.content[self.selected_range.clone()].to_string(),
            ));
        }
    }
    fn cut(&mut self, _: &Cut, window: &mut Window, cx: &mut Context<Self>) {
        if !self.selected_range.is_empty() {
            cx.write_to_clipboard(ClipboardItem::new_string(
                self.content[self.selected_range.clone()].to_string(),
            ));
            self.replace_text_in_range(None, "", window, cx)
        }
    }

    fn move_to(&mut self, offset: usize, window: &Window, cx: &mut Context<Self>) {
        self.selected_range = offset..offset;
        self.selection_reversed = false;
        self.reveal = true;
        window.notify_text_selection_changed(&self.focus_handle);
        cx.notify()
    }

    fn cursor_offset(&self) -> usize {
        if self.selection_reversed {
            self.selected_range.start
        } else {
            self.selected_range.end
        }
    }

    fn index_for_mouse_position(&self, position: Point<Pixels>) -> usize {
        if self.content.is_empty() {
            return 0;
        }

        let (Some(bounds), Some(layout)) = (self.last_bounds.as_ref(), self.last_layout.as_ref())
        else {
            return 0;
        };
        if position.y < bounds.top() {
            return 0;
        }
        if position.y > bounds.bottom() {
            return self.content.len();
        }
        self.from_display(layout.index_at(position - bounds.origin))
    }

    fn select_to(&mut self, offset: usize, window: &Window, cx: &mut Context<Self>) {
        self.reveal = true;
        if self.selection_reversed {
            self.selected_range.start = offset
        } else {
            self.selected_range.end = offset
        };
        if self.selected_range.end < self.selected_range.start {
            self.selection_reversed = !self.selection_reversed;
            self.selected_range = self.selected_range.end..self.selected_range.start;
        }
        window.notify_text_selection_changed(&self.focus_handle);
        cx.notify()
    }

    fn offset_from_utf16(&self, offset: usize) -> usize {
        let mut utf8_offset = 0;
        let mut utf16_count = 0;

        for ch in self.content.chars() {
            if utf16_count >= offset {
                break;
            }
            utf16_count += ch.len_utf16();
            utf8_offset += ch.len_utf8();
        }

        utf8_offset
    }

    fn offset_to_utf16(&self, offset: usize) -> usize {
        let mut utf16_offset = 0;
        let mut utf8_count = 0;

        for ch in self.content.chars() {
            if utf8_count >= offset {
                break;
            }
            utf8_count += ch.len_utf8();
            utf16_offset += ch.len_utf16();
        }

        utf16_offset
    }

    fn range_to_utf16(&self, range: &Range<usize>) -> Range<usize> {
        self.offset_to_utf16(range.start)..self.offset_to_utf16(range.end)
    }

    fn range_from_utf16(&self, range_utf16: &Range<usize>) -> Range<usize> {
        self.offset_from_utf16(range_utf16.start)..self.offset_from_utf16(range_utf16.end)
    }

    fn previous_boundary(&self, offset: usize) -> usize {
        let at = self
            .content
            .grapheme_indices(true)
            .rev()
            .find_map(|(idx, _)| (idx < offset).then_some(idx))
            .unwrap_or(0);
        self.out_of_label(at, false)
    }

    fn next_boundary(&self, offset: usize) -> usize {
        let at = self
            .content
            .grapheme_indices(true)
            .find_map(|(idx, _)| (idx > offset).then_some(idx))
            .unwrap_or(self.content.len());
        self.out_of_label(at, true)
    }

    /// Keep the state before an edit for Undo; a run of typing keeps only its first.
    fn remember(&mut self, typing: bool) {
        let now = std::time::Instant::now();
        let joins = typing && self.typed.is_some_and(|t| now.duration_since(t).as_millis() < 1000);
        if !joins {
            self.undo.push((self.content.clone(), self.selected_range.clone()));
            if self.undo.len() > 200 {
                self.undo.remove(0);
            }
        }
        self.typed = typing.then_some(now);
        self.redo.clear();
    }

    fn forget(&mut self) {
        self.undo.clear();
        self.redo.clear();
        self.typed = None;
    }

    fn restore(&mut self, (content, sel): (SharedString, Range<usize>), window: &Window, cx: &mut Context<Self>) {
        self.content = content;
        self.selected_range = sel.start.min(self.content.len())..sel.end.min(self.content.len());
        self.selection_reversed = false;
        self.marked_range = None;
        self.typed = None;
        self.reveal = true;
        window.notify_text_input_changed(&self.focus_handle);
        cx.emit(InputChanged);
        cx.notify();
    }

    fn undo(&mut self, _: &Undo, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(prev) = self.undo.pop() {
            self.redo.push((self.content.clone(), self.selected_range.clone()));
            self.restore(prev, window, cx);
        }
    }

    fn redo(&mut self, _: &Redo, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(next) = self.redo.pop() {
            self.undo.push((self.content.clone(), self.selected_range.clone()));
            self.restore(next, window, cx);
        }
    }

    /// Where the word before `offset` starts: past spaces, then a run of word characters or
    /// of punctuation.
    fn word_start(&self, offset: usize) -> usize {
        let s = &self.content[..offset];
        let mut it = s.char_indices().rev().skip_while(|(_, c)| c.is_whitespace()).peekable();
        let Some(&(mut at, first)) = it.peek() else { return 0 };
        let word = first.is_alphanumeric() || first == '_';
        for (i, c) in it {
            if c.is_whitespace() || (c.is_alphanumeric() || c == '_') != word {
                break;
            }
            at = i;
        }
        self.out_of_label(at, false)
    }

    fn word_end(&self, offset: usize) -> usize {
        let s = &self.content[offset..];
        let mut it = s.char_indices().skip_while(|(_, c)| c.is_whitespace()).peekable();
        let Some(&(_, first)) = it.peek() else { return self.content.len() };
        let word = first.is_alphanumeric() || first == '_';
        let mut end = self.content.len();
        for (i, c) in it {
            if c.is_whitespace() || (c.is_alphanumeric() || c == '_') != word {
                end = offset + i;
                break;
            }
        }
        self.out_of_label(end, true)
    }

    fn word_left(&mut self, _: &WordLeft, window: &mut Window, cx: &mut Context<Self>) {
        self.move_to(self.word_start(self.cursor_offset()), window, cx);
    }

    fn word_right(&mut self, _: &WordRight, window: &mut Window, cx: &mut Context<Self>) {
        self.move_to(self.word_end(self.cursor_offset()), window, cx);
    }

    fn select_word_left(&mut self, _: &SelectWordLeft, window: &mut Window, cx: &mut Context<Self>) {
        self.select_to(self.word_start(self.cursor_offset()), window, cx);
    }

    fn select_word_right(&mut self, _: &SelectWordRight, window: &mut Window, cx: &mut Context<Self>) {
        self.select_to(self.word_end(self.cursor_offset()), window, cx);
    }

    fn delete_word_left(&mut self, _: &DeleteWordLeft, window: &mut Window, cx: &mut Context<Self>) {
        if self.selected_range.is_empty() {
            self.select_to(self.word_start(self.cursor_offset()), window, cx);
        }
        self.replace_text_in_range(None, "", window, cx)
    }

    fn delete_word_right(&mut self, _: &DeleteWordRight, window: &mut Window, cx: &mut Context<Self>) {
        if self.selected_range.is_empty() {
            self.select_to(self.word_end(self.cursor_offset()), window, cx);
        }
        self.replace_text_in_range(None, "", window, cx)
    }

    fn select_home(&mut self, _: &SelectHome, window: &mut Window, cx: &mut Context<Self>) {
        self.select_to(0, window, cx);
    }

    fn select_end(&mut self, _: &SelectEnd, window: &mut Window, cx: &mut Context<Self>) {
        self.select_to(self.content.len(), window, cx);
    }

    /// A formatting shortcut: markdown written into the composer's text, as on desktop.
    fn mark(&mut self, mark: &str, window: &mut Window, cx: &mut Context<Self>) {
        if self.decorated.is_none() || self.marked_range.is_some() {
            return cx.propagate();
        }
        let (text, sel) = crate::marks::toggle(&self.content, self.selected_range.clone(), mark);
        self.remember(false);
        self.restore((text.into(), sel), window, cx);
    }

    fn reset(&mut self, window: &Window, cx: &mut Context<Self>) {
        self.forget();
        self.content = "".into();
        self.selected_range = 0..0;
        self.selection_reversed = false;
        self.marked_range = None;
        self.last_layout = None;
        self.last_bounds = None;
        self.is_selecting = false;
        window.notify_text_input_changed(&self.focus_handle);
        cx.emit(InputChanged);
        cx.notify();
    }
}

impl EntityInputHandler for TextInput {
    fn text_for_range(
        &mut self,
        range_utf16: Range<usize>,
        actual_range: &mut Option<Range<usize>>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<String> {
        let range = self.range_from_utf16(&range_utf16);
        actual_range.replace(self.range_to_utf16(&range));
        Some(self.content[range].to_string())
    }

    fn selected_text_range(
        &mut self,
        _ignore_disabled_input: bool,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<UTF16Selection> {
        Some(UTF16Selection {
            range: self.range_to_utf16(&self.selected_range),
            reversed: self.selection_reversed,
        })
    }

    fn set_selected_text_range(
        &mut self,
        range_utf16: Range<usize>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.selected_range = self.range_from_utf16(&range_utf16);
        self.selection_reversed = false;
        window.notify_text_selection_changed(&self.focus_handle);
        cx.notify();
    }

    fn text_length_utf16(
        &mut self,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<usize> {
        Some(self.content.encode_utf16().count())
    }

    fn marked_text_range(
        &self,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<Range<usize>> {
        self.marked_range
            .as_ref()
            .map(|range| self.range_to_utf16(range))
    }

    fn unmark_text(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.marked_range = None;
        window.notify_text_input_changed(&self.focus_handle);
        cx.notify();
    }

    fn replace_text_in_range(
        &mut self,
        range_utf16: Option<Range<usize>>,
        new_text: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let range = range_utf16
            .as_ref()
            .map(|range_utf16| self.range_from_utf16(range_utf16))
            .or(self.marked_range.clone())
            .unwrap_or(self.selected_range.clone());
        let new_text = match self.max_chars {
            Some(max) => {
                let kept = self.content[..range.start].chars().count()
                    + self.content[range.end..].chars().count();
                let room = max.saturating_sub(kept);
                new_text
                    .char_indices()
                    .nth(room)
                    .map_or(new_text, |(at, _)| &new_text[..at])
            }
            None => new_text,
        };
        let typing = range.is_empty()
            && self.marked_range.is_none()
            && new_text.graphemes(true).count() == 1
            && !new_text.chars().any(char::is_whitespace);
        self.remember(typing);

        self.content =
            (self.content[0..range.start].to_owned() + new_text + &self.content[range.end..])
                .into();
        self.selected_range = range.start + new_text.len()..range.start + new_text.len();
        self.selection_reversed = false;
        self.marked_range.take();
        self.reveal = true;
        window.notify_text_input_changed(&self.focus_handle);
        cx.emit(InputChanged);
        cx.notify();
    }

    fn replace_and_mark_text_in_range(
        &mut self,
        range_utf16: Option<Range<usize>>,
        new_text: &str,
        new_selected_range_utf16: Option<Range<usize>>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let range = range_utf16
            .as_ref()
            .map(|range_utf16| self.range_from_utf16(range_utf16))
            .or(self.marked_range.clone())
            .unwrap_or(self.selected_range.clone());

        self.content =
            (self.content[0..range.start].to_owned() + new_text + &self.content[range.end..])
                .into();
        if !new_text.is_empty() {
            self.marked_range = Some(range.start..range.start + new_text.len());
        } else {
            self.marked_range = None;
        }
        let insertion_start_utf16 = self.offset_to_utf16(range.start);
        let inserted_length_utf16 = new_text.encode_utf16().count();
        self.selected_range = new_selected_range_utf16
            .map(|selected| {
                self.range_from_utf16(
                    &(insertion_start_utf16 + selected.start.min(inserted_length_utf16)
                        ..insertion_start_utf16 + selected.end.min(inserted_length_utf16)),
                )
            })
            .unwrap_or_else(|| range.start + new_text.len()..range.start + new_text.len());
        self.selection_reversed = false;

        window.notify_text_input_changed(&self.focus_handle);
        cx.notify();
    }

    fn bounds_for_range(
        &mut self,
        range_utf16: Range<usize>,
        bounds: Bounds<Pixels>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<Bounds<Pixels>> {
        let layout = self.last_layout.as_ref()?;
        if self.look.masked || layout.text != self.content {
            return None;
        }
        let range = self.range_from_utf16(&range_utf16);
        let (a, b) = (layout.position(range.start), layout.position(range.end));
        let b = if b.y == a.y { b } else { point(a.x, a.y) };
        Some(Bounds::from_corners(
            bounds.origin + a,
            bounds.origin + point(b.x, a.y + layout.line_height),
        ))
    }

    fn selection_bounds_for_range(
        &mut self,
        range_utf16: Range<usize>,
        bounds: Bounds<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Vec<Bounds<Pixels>> {
        self.bounds_for_range(range_utf16, bounds, window, cx)
            .into_iter()
            .collect()
    }

    fn character_index_for_point(
        &mut self,
        point: gpui::Point<Pixels>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<usize> {
        let local = self.last_bounds?.localize(&point)?;
        let layout = self.last_layout.as_ref()?;
        if layout.text != self.content {
            return None;
        }
        Some(self.offset_to_utf16(layout.index_at(local)))
    }
}

struct TextElement {
    input: Entity<TextInput>,
}

struct PrepaintState {
    /// The text's box, shifted left by a one-line field's scroll.
    text: Bounds<Pixels>,
    layout: Option<Layout>,
    emoji: Vec<gpui::AnyElement>,
    cursor: Option<PaintQuad>,
    selections: Vec<PaintQuad>,
}

impl TextInput {
    /// What is drawn (placeholder, bullets or the text) and its runs, in the element's style.
    fn display(&self, window: &Window) -> (SharedString, Vec<TextRun>, Pixels) {
        let mut style = window.text_style();
        let content = self.content.clone();
        let (text, color) = if content.is_empty() {
            (self.placeholder.clone(), self.look.placeholder)
        } else if self.look.masked {
            if let Some(size) = self.look.masked_size {
                style.font_size = size.into();
            }
            (
                BULLET.repeat(content.graphemes(true).count()).into(),
                style.color,
            )
        } else {
            let subs = self.subs();
            if subs.is_empty() {
                (content, style.color)
            } else {
                let mut text = String::with_capacity(content.len());
                let mut at = 0;
                for sub in subs.iter() {
                    text.push_str(&content[at..sub.content.start]);
                    text.push_str(sub.label.as_deref().unwrap_or(EMOJI_SLOT));
                    at = sub.content.end;
                }
                text.push_str(&content[at..]);
                (text.into(), style.color)
            }
        };
        let run = TextRun {
            len: text.len(),
            font: style.font(),
            color,
            background_color: None,
            underline: None,
            strikethrough: None,
        };
        let marked = self
            .marked_range
            .as_ref()
            .filter(|_| !self.look.masked && !self.content.is_empty())
            .map(|m| self.to_display(m.start)..self.to_display(m.end));
        let runs = match marked.as_ref() {
            Some(m) => [
                TextRun {
                    len: m.start,
                    ..run.clone()
                },
                TextRun {
                    len: m.end - m.start,
                    underline: Some(UnderlineStyle {
                        color: Some(run.color),
                        thickness: px(1.0),
                        wavy: false,
                    }),
                    ..run.clone()
                },
                TextRun {
                    len: text.len() - m.end,
                    ..run
                },
            ]
            .into_iter()
            .filter(|r| r.len > 0)
            .collect(),
            None => vec![run],
        };
        self.chips.borrow_mut().clear();
        let runs = match self.decorated.as_ref().filter(|_| !self.content.is_empty() && !self.look.masked) {
            Some(names) => self.decorate(runs, names),
            None => runs,
        };
        (text, runs, style.font_size.to_pixels(window.rem_size()))
    }

    /// Splits `runs` at each decorated span and styles what falls inside.
    fn decorate(&self, runs: Vec<TextRun>, names: &[String]) -> Vec<TextRun> {
        use crate::decorate::Deco;
        let subs = self.subs();
        let labels: Vec<&Sub> = subs.iter().filter(|s| s.label.is_some()).collect();
        let tokens: Vec<(std::ops::Range<usize>, Deco)> = crate::decorate::tokenize(&self.content, names)
            .into_iter()
            .filter(|(r, _)| !labels.iter().any(|s| r.start < s.content.end && s.content.start < r.end))
            .collect();
        let (marks, paint) = match self.colors {
            Some(owned) => {
                let skip: Vec<std::ops::Range<usize>> = tokens
                    .iter()
                    .filter(|(_, d)| matches!(d, Deco::Code | Deco::CodeMark | Deco::Mention | Deco::Spoiler))
                    .map(|(r, _)| r.clone())
                    .chain(subs.iter().map(|s| s.content.clone()))
                    .collect();
                crate::textcolor::composer_paint(&self.content, owned, &skip)
            }
            None => Default::default(),
        };
        let disp = |r: &std::ops::Range<usize>| self.to_display(r.start)..self.to_display(r.end);
        let paint: Vec<(std::ops::Range<usize>, Hsla)> = paint.iter().map(|(r, c)| (disp(r), *c)).collect();
        let mut spans: Vec<(std::ops::Range<usize>, Deco)> = tokens
            .iter()
            .map(|(r, d)| (disp(r), *d))
            .chain(labels.iter().map(|s| (s.display.clone(), Deco::Mention)))
            .chain(marks.iter().map(|r| (disp(r), Deco::Mark)))
            .collect();
        if spans.is_empty() {
            return runs;
        }
        spans.sort_by_key(|(r, _)| r.start);
        *self.chips.borrow_mut() = spans
            .iter()
            .filter_map(|(r, d)| match d {
                Deco::Mention => Some((r.clone(), Hsla::from(gpui::Rgba { r: 89. / 255., g: 252. / 255., b: 179. / 255., a: 0.13 }), 4., 2.)),
                Deco::Code => Some((r.clone(), gpui::hsla(0., 0., 1., 0.08), 3., 2.)),
                Deco::Spoiler => Some((r.clone(), gpui::hsla(0., 0., 1., 0.16), 3., 0.)),
                _ => None,
            })
            .collect();
        let mut out = Vec::with_capacity(runs.len() + spans.len() * 2);
        let mut at = 0;
        for run in runs {
            let end = at + run.len;
            let mut from = at;
            for (r, d) in spans.iter().filter(|(r, _)| r.start < end && r.end > at) {
                let (s, e) = (r.start.max(at), r.end.min(end));
                if s > from {
                    out.push(TextRun { len: s - from, ..run.clone() });
                }
                let mut styled = TextRun { len: e - s, ..run.clone() };
                let base = run.color;
                match d {
                    Deco::Mark => styled.color = base.opacity(0.4),
                    Deco::CodeMark => {
                        styled.color = base.opacity(0.4);
                        styled.font.family = crate::markdown::MONO.into();
                    }
                    Deco::Bold | Deco::Header => styled.font.weight = gpui::FontWeight::BOLD,
                    Deco::Italic => {
                        styled.font.style = gpui::FontStyle::Italic;
                        styled.color = base.opacity(0.7);
                    }
                    Deco::BoldItalic => {
                        styled.font.weight = gpui::FontWeight::BOLD;
                        styled.font.style = gpui::FontStyle::Italic;
                    }
                    Deco::Strike => {
                        styled.strikethrough = Some(gpui::StrikethroughStyle {
                            thickness: px(1.),
                            color: Some(base),
                        })
                    }
                    Deco::Spoiler => {}
                    Deco::Code => styled.font.family = crate::markdown::MONO.into(),
                    Deco::Mention => {
                        styled.color = gpui::rgb(crate::theme::PRIMARY).into();
                        styled.font.weight = gpui::FontWeight::MEDIUM;
                    }
                    Deco::Subtext => styled.color = base.opacity(0.75),
                }
                out.push(styled);
                from = e;
            }
            if end > from {
                out.push(TextRun { len: end - from, ..run });
            }
            at = end;
        }
        if paint.is_empty() {
            return out;
        }
        let mut painted = Vec::with_capacity(out.len() + paint.len() * 2);
        let mut at = 0;
        for run in out {
            let end = at + run.len;
            let mut from = at;
            for (r, color) in paint.iter().filter(|(r, _)| r.start < end && r.end > at) {
                let (s, e) = (r.start.max(at), r.end.min(end));
                if s > from {
                    painted.push(TextRun { len: s - from, ..run.clone() });
                }
                painted.push(TextRun { len: e - s, color: Hsla { a: run.color.a, ..*color }, ..run.clone() });
                from = e;
            }
            if end > from {
                painted.push(TextRun { len: end - from, ..run });
            }
            at = end;
        }
        painted
    }
}

impl Element for TextElement {
    type RequestLayoutState = ();
    type PrepaintState = PrepaintState;

    fn id(&self) -> Option<ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static core::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&gpui::InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, Self::RequestLayoutState) {
        let input = self.input.read(cx);
        let line_height = window.line_height();
        let mut style = Style::default();
        style.size.width = relative(1.).into();
        if input.multiline.is_none() {
            style.size.height = line_height.into();
            return (window.request_layout(style, [], cx), ());
        }
        let (text, runs, font_size) = input.display(window);
        let id = window.request_measured_layout(style, move |known, available, window, _| {
            let width = known.width.or(match available.width {
                gpui::AvailableSpace::Definite(w) => Some(w),
                _ => None,
            });
            let layout = Layout::shape(text.clone(), &runs, font_size, width, line_height, window);
            size(width.unwrap_or(px(0.)), layout.height())
        });
        (id, ())
    }

    fn prepaint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&gpui::InspectorElementId>,
        bounds: Bounds<Pixels>,
        _request_layout: &mut Self::RequestLayoutState,
        window: &mut Window,
        cx: &mut App,
    ) -> Self::PrepaintState {
        let input = self.input.read(cx);
        let (text, runs, font_size) = input.display(window);
        let line_height = window.line_height();
        let wrap = input.multiline.map(|_| bounds.size.width);
        let mut layout = Layout::shape(text, &runs, font_size, wrap, line_height, window);
        layout.align = (input.center || input.right).then_some(bounds.size.width);
        layout.right = input.right;
        let (start, end) = if input.content.is_empty() {
            (0, 0)
        } else {
            (
                input.to_display(input.selected_range.start),
                input.to_display(input.selected_range.end),
            )
        };
        let caret = if input.content.is_empty() {
            0
        } else {
            input.to_display(input.cursor_offset())
        };
        let field = bounds;
        let bounds = if input.multiline.is_none() && layout.align.is_none() {
            let w = bounds.size.width - px(2.);
            let text_w = layout.lines.first().map(|(_, _, l)| l.width()).unwrap_or_default();
            let x = layout.position(caret).x;
            let mut sx = input.hscroll.get();
            if x - sx > w {
                sx = x - w;
            } else if x < sx {
                sx = x;
            }
            sx = sx.min((text_w - w).max(px(0.))).max(px(0.));
            input.hscroll.set(sx);
            Bounds::new(field.origin - point(sx, px(0.)), size(field.size.width + sx, field.size.height))
        } else {
            field
        };
        let o = bounds.origin;
        let (cursor, selections) = if start == end {
            let p = layout.position(caret);
            (
                Some(fill(
                    Bounds::new(o + p, size(px(2.), line_height)),
                    c(ACCENT),
                )),
                Vec::new(),
            )
        } else {
            let (a, b) = (layout.position(start), layout.position(end));
            let tint = ca(89, 252, 179, 0.25);
            let mut rects = Vec::new();
            if a.y == b.y {
                rects.push(Bounds::from_corners(
                    o + a,
                    o + point(b.x, b.y + line_height),
                ));
            } else {
                rects.push(Bounds::from_corners(
                    o + a,
                    o + point(bounds.size.width, a.y + line_height),
                ));
                if b.y > a.y + line_height {
                    rects.push(Bounds::from_corners(
                        o + point(px(0.), a.y + line_height),
                        o + point(bounds.size.width, b.y),
                    ));
                }
                rects.push(Bounds::from_corners(
                    o + point(px(0.), b.y),
                    o + point(b.x, b.y + line_height),
                ));
            }
            (None, rects.into_iter().map(|r| fill(r, tint)).collect())
        };
        let side = font_size * 1.25;
        let emoji = input
            .subs()
            .iter()
            .filter_map(|sub| Some((sub, sub.src.clone()?)))
            .map(|(sub, src)| {
                let p = layout.position(sub.display.start);
                let mut art = crate::pic::img(src).size(side).into_any_element();
                let at = o + point(p.x, p.y + (line_height - side) / 2.);
                art.prepaint_as_root(
                    at,
                    size(side, side).map(gpui::AvailableSpace::Definite),
                    window,
                    cx,
                );
                art
            })
            .collect();
        PrepaintState {
            text: bounds,
            layout: Some(layout),
            emoji,
            cursor,
            selections,
        }
    }

    fn paint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&gpui::InspectorElementId>,
        bounds: Bounds<Pixels>,
        _request_layout: &mut Self::RequestLayoutState,
        prepaint: &mut Self::PrepaintState,
        window: &mut Window,
        cx: &mut App,
    ) {
        let focus_handle = self.input.read(cx).focus_handle.clone();
        window.handle_input(
            &focus_handle,
            ElementInputHandler::new(bounds, self.input.clone()),
            cx,
        );
        let field = bounds;
        let bounds = prepaint.text;
        let mask = gpui::ContentMask {
            bounds: Bounds::new(field.origin - point(px(0.), px(40.)), size(field.size.width, field.size.height + px(80.))),
        };
        let (layout, caret) = window.with_content_mask(Some(mask), |window| {
            for quad in prepaint.selections.drain(..) {
                window.paint_quad(quad);
            }
            let layout = prepaint.layout.take().unwrap();
            // A chip spans the text's 18px content box, not the 21px line.
            let inset = ((layout.line_height - px(18.)) / 2.).max(px(0.));
            for (range, fill, radius, pad) in self.input.read(cx).chips.borrow().iter() {
                let (a, b) = (layout.position(range.start), layout.position(range.end));
                let rows = if a.y == b.y {
                    vec![(a.x, b.x, a.y)]
                } else {
                    vec![(a.x, bounds.size.width, a.y), (px(0.), b.x, b.y)]
                };
                for (x0, x1, y) in rows {
                    let quad = Bounds::new(
                        bounds.origin + point(x0 - px(*pad), y + inset),
                        gpui::size(x1 - x0 + px(2. * pad), layout.line_height - inset * 2.),
                    );
                    window.paint_quad(gpui::fill(quad, *fill).corner_radii(gpui::Corners::all(px(*radius))));
                }
            }
            for (_, y, line) in &layout.lines {
                let align = if layout.right {
                    gpui::TextAlign::Right
                } else if layout.align.is_some() {
                    gpui::TextAlign::Center
                } else {
                    gpui::TextAlign::Left
                };
                line.paint(
                    bounds.origin + point(px(0.), *y),
                    layout.line_height,
                    align,
                    layout.align.map(|_| bounds),
                    window,
                    cx,
                )
                .ok();
            }
            for art in &mut prepaint.emoji {
                art.paint(window, cx);
            }
            let caret = prepaint.cursor.take();
            let focused = focus_handle.is_focused(window);
            if focused && let Some(cursor) = caret.clone() {
                window.paint_quad(cursor);
            }
            (layout, caret)
        });
        self.input.update(cx, |input, cx| {
            input.last_layout = Some(layout);
            input.last_bounds = Some(bounds);
            // Keep the caret in view of a scrolled field.
            if input.reveal && input.multiline.is_some() {
                input.reveal = false;
                if let Some(cursor) = caret {
                    let view = input.scroll.bounds();
                    let mut offset = input.scroll.offset();
                    let (top, bottom) = (cursor.bounds.top(), cursor.bounds.bottom());
                    if top < view.top() {
                        offset.y += view.top() - top;
                    } else if bottom > view.bottom() {
                        offset.y -= bottom - view.bottom();
                    } else {
                        return;
                    }
                    input.scroll.set_offset(offset);
                    cx.notify();
                }
            }
        });
    }
}

impl Render for TextInput {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .id("text-input")
            .flex()
            .key_context("TextInput")
            .track_focus(&self.focus_handle(cx))
            .cursor(CursorStyle::IBeam)
            .on_action(cx.listener(Self::backspace))
            .on_action(cx.listener(Self::delete))
            .on_action(cx.listener(Self::left))
            .on_action(cx.listener(Self::right))
            .on_action(cx.listener(Self::select_left))
            .on_action(cx.listener(Self::select_right))
            .on_action(cx.listener(Self::select_all))
            .on_action(cx.listener(Self::home))
            .on_action(cx.listener(Self::end))
            .on_action(cx.listener(Self::show_character_palette))
            .on_action(cx.listener(Self::paste))
            .on_action(cx.listener(Self::cut))
            .on_action(cx.listener(Self::copy))
            .on_action(cx.listener(Self::newline))
            .on_action(cx.listener(Self::undo))
            .on_action(cx.listener(Self::redo))
            .on_action(cx.listener(Self::word_left))
            .on_action(cx.listener(Self::word_right))
            .on_action(cx.listener(Self::select_word_left))
            .on_action(cx.listener(Self::select_word_right))
            .on_action(cx.listener(Self::delete_word_left))
            .on_action(cx.listener(Self::delete_word_right))
            .on_action(cx.listener(Self::select_home))
            .on_action(cx.listener(Self::select_end))
            .on_action(cx.listener(|this, _: &Bold, w, cx| this.mark("**", w, cx)))
            .on_action(cx.listener(|this, _: &Italic, w, cx| this.mark("*", w, cx)))
            .on_action(cx.listener(|this, _: &Code, w, cx| this.mark("`", w, cx)))
            .on_action(cx.listener(|this, _: &Strike, w, cx| this.mark("~~", w, cx)))
            .on_action(cx.listener(Self::send_now))
            .on_action(cx.listener(|this, _: &Up, w, cx| {
                if this.nav {
                    return cx.emit(Nav::Up);
                }
                this.vertical(-1., false, w, cx)
            }))
            .on_action(cx.listener(|this, _: &Down, w, cx| {
                if this.nav {
                    return cx.emit(Nav::Down);
                }
                this.vertical(1., false, w, cx)
            }))
            .on_action(cx.listener(|this, _: &Tab, _, cx| {
                if this.nav || this.chip {
                    cx.emit(Nav::Pick);
                } else {
                    cx.propagate();
                }
            }))
            .on_action(cx.listener(|this, _: &SelectUp, w, cx| this.vertical(-1., true, w, cx)))
            .on_action(cx.listener(|this, _: &SelectDown, w, cx| this.vertical(1., true, w, cx)))
            .on_mouse_down(MouseButton::Left, cx.listener(Self::on_mouse_down))
            .on_mouse_up(MouseButton::Left, cx.listener(Self::on_mouse_up))
            .on_mouse_up_out(MouseButton::Left, cx.listener(Self::on_mouse_up))
            .on_mouse_move(cx.listener(Self::on_mouse_move))
            .on_action(cx.listener(Self::send))
            .on_action(cx.listener(Self::cancel))
            .w_full()
            .when_some(self.multiline, |d, max| {
                d.max_h(max).overflow_y_scroll().track_scroll(&self.scroll)
            })
            .font_family(self.look.family)
            .font_weight(self.look.weight)
            .line_height(self.look.line_height)
            .text_size(self.look.size)
            .text_color(self.look.color)
            .child(TextElement { input: cx.entity() })
    }
}

impl Focusable for TextInput {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

pub struct SendText(pub String);
/// The text changed.
pub struct InputChanged;
impl EventEmitter<InputChanged> for TextInput {}

/// A key that would leave the field: Left at its start, Right at its end, Backspace or
/// Delete when empty. Only emitted with `edges`, for a row of fields walked as one.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Edge {
    Start,
    End,
    Empty,
}
impl EventEmitter<Edge> for TextInput {}
/// Escape in the field.
pub struct CancelInput;

/// A paste that carried no text but a picture or a file.
pub enum Pasted {
    Image { bytes: Vec<u8>, ext: &'static str },
    Paths(Vec<std::path::PathBuf>),
}

impl EventEmitter<SendText> for TextInput {}
impl EventEmitter<Nav> for TextInput {}
impl EventEmitter<CancelInput> for TextInput {}
impl EventEmitter<Pasted> for TextInput {}

impl TextInput {
    pub fn new(placeholder: impl Into<SharedString>, cx: &mut Context<Self>) -> Self {
        Self {
            focus_handle: cx.focus_handle(),
            content: "".into(),
            placeholder: placeholder.into(),
            selected_range: 0..0,
            selection_reversed: false,
            marked_range: None,
            last_layout: None,
            last_bounds: None,
            hscroll: Default::default(),
            multiline: None,
            center: false,
            right: false,
            scroll: ScrollHandle::new(),
            reveal: false,
            is_selecting: false,
            look: Look::default(),
            clear_on_submit: true,
            submit_empty: false,
            twemoji: false,
            max_chars: None,
            subs: Default::default(),
            nav: false,
            edges: false,
            colors: None,
            chip: false,
            decorated: None,
            npub_label: None,
            customs: Default::default(),
            chips: Default::default(),
            undo: Vec::new(),
            redo: Vec::new(),
            typed: None,
        }
    }

    /// Styles markdown, headers and tags in place, as the rich composer does.
    pub fn decorated(mut self) -> Self {
        self.decorated = Some(Vec::new());
        self
    }

    /// The names a picked tag shows as, so `@Name` pills.
    pub fn set_mentions(&mut self, names: Vec<String>, cx: &mut Context<Self>) {
        if self.decorated.as_ref() != Some(&names) {
            self.decorated = Some(names);
            cx.notify();
        }
    }

    /// Draws stock emoji as Twemoji art.
    pub fn twemoji(mut self) -> Self {
        self.twemoji = true;
        self
    }

    fn subs(&self) -> std::rc::Rc<[Sub]> {
        if (!self.twemoji && self.npub_label.is_none()) || self.look.masked {
            return std::rc::Rc::from([]);
        }
        let mut cache = self.subs.borrow_mut();
        // Names can change under the same text, so a draft holding an npub resolves each time.
        let named = self.npub_label.is_some() && self.content.contains("@npub1");
        if cache.0 != self.content || named {
            let npubs = match self.npub_label.as_ref() {
                Some(label) if self.content.contains("@npub1") => npub_mentions(&self.content)
                    .into_iter()
                    .map(|r| {
                        let name = label(&self.content[r.start + 1..r.end]);
                        (r, format!("@{name}"))
                    })
                    .collect(),
                _ => Vec::new(),
            };
            let mut found: Vec<(Range<usize>, Option<gpui::ImageSource>, Option<String>)> =
                npubs.into_iter().map(|(r, l)| (r, None, Some(l))).collect();
            let mut settled = true;
            if self.twemoji {
                for (i, g) in self.content.grapheme_indices(true) {
                    if let Some(src) = crate::emoji::twemoji(g) {
                        found.push((i..i + g.len(), Some(src.into()), None));
                    }
                }
                if !self.customs.is_empty() && self.content.contains(':') {
                    let media = crate::media::shared();
                    for (a, b) in crate::markdown::shortcodes(&self.content) {
                        let Some(url) = self.customs.get(&self.content[a + 1..b - 1]) else { continue };
                        match media.borrow_mut().custom(url, false) {
                            crate::media::Custom::Ready(image) => found.push((a..b, Some(gpui::ImageSource::Render(image)), None)),
                            // Held as a blank slot until it lands, then drawn.
                            crate::media::Custom::Pending => {
                                settled = false;
                                found.push((a..b, None, None));
                            }
                            crate::media::Custom::Missing(_) => {}
                        }
                    }
                }
                found.sort_by_key(|f| f.0.start);
            }
            let mut subs = Vec::new();
            let mut shift = 0isize;
            for (content, src, label) in found {
                let shown = label.as_deref().unwrap_or(EMOJI_SLOT).len();
                let start = (content.start as isize + shift) as usize;
                shift += shown as isize - content.len() as isize;
                subs.push(Sub {
                    content,
                    display: start..start + shown,
                    src,
                    label: label.map(Into::into),
                });
            }
            *cache = (if settled { self.content.clone() } else { SharedString::default() }, subs.into());
        }
        cache.1.clone()
    }

    /// The pack emoji this field draws as art, by shortcode.
    pub fn set_customs(&mut self, customs: std::rc::Rc<std::collections::HashMap<String, String>>, cx: &mut Context<Self>) {
        if *self.customs != *customs {
            self.customs = customs;
            self.subs.borrow_mut().0 = SharedString::default();
            cx.notify();
        }
    }

    /// Shows a pasted `@npub1…` as `@` and what `label` names it.
    pub fn set_npub_labels(&mut self, label: std::rc::Rc<dyn Fn(&str) -> String>) {
        self.npub_label = Some(label);
        self.subs.borrow_mut().0 = SharedString::default();
    }

    /// `offset` moved out of any name drawn over an npub, toward `forward`'s side.
    fn out_of_label(&self, offset: usize, forward: bool) -> usize {
        self.subs()
            .iter()
            .find(|s| s.label.is_some() && s.content.start < offset && offset < s.content.end)
            .map_or(offset, |s| if forward { s.content.end } else { s.content.start })
    }

    /// Wraps, takes Shift+Enter, and scrolls once taller than `max_height`.
    /// `text-align: center`.
    pub fn centered(mut self) -> Self {
        self.center = true;
        self
    }

    /// `text-align: right`.
    pub fn right_aligned(mut self) -> Self {
        self.right = true;
        self
    }

    pub fn multiline(mut self, max_height: Pixels) -> Self {
        self.multiline = Some(max_height);
        self
    }

    /// Types `text` at the caret, spaced from the words either side (`insertAtCursor`).
    pub fn insert_spaced(&mut self, text: &str, window: &mut Window, cx: &mut Context<Self>) {
        let (start, end) = (self.selected_range.start, self.selected_range.end);
        let before = self.content[..start].chars().next_back();
        let after = self.content[end..].chars().next();
        let mut t = String::new();
        if before.is_some_and(|c| !c.is_whitespace()) {
            t.push(' ');
        }
        t.push_str(text);
        if after.is_some_and(|c| !c.is_whitespace()) {
            t.push(' ');
        }
        self.replace_text_in_range(None, &t, window, cx);
    }

    fn newline(&mut self, _: &Newline, window: &mut Window, cx: &mut Context<Self>) {
        if self.multiline.is_some() {
            self.replace_text_in_range(None, "\n", window, cx);
        }
    }

    /// Up or down a visual line, keeping the caret's x.
    fn vertical(&mut self, dir: f32, select: bool, window: &mut Window, cx: &mut Context<Self>) {
        let Some(layout) = self
            .last_layout
            .as_ref()
            .filter(|_| self.multiline.is_some())
        else {
            return;
        };
        let p = layout.position(self.to_display(self.cursor_offset()));
        let y = p.y + layout.line_height * dir + layout.line_height / 2.;
        let target = if y < px(0.) {
            0
        } else if y >= layout.height() {
            self.content.len()
        } else {
            self.from_display(layout.index_at(point(p.x, y)))
        };
        if select {
            self.select_to(target, window, cx);
        } else {
            self.move_to(target, window, cx);
        }
    }

    /// Enter submits an empty field too.
    pub fn edges(mut self) -> Self {
        self.edges = true;
        self
    }

    pub fn submit_empty(mut self) -> Self {
        self.submit_empty = true;
        self
    }

    pub fn keep_on_submit(mut self) -> Self {
        self.clear_on_submit = false;
        self.submit_empty = true;
        self
    }

    pub fn max_chars(mut self, n: usize) -> Self {
        self.max_chars = Some(n);
        self
    }

    pub fn with_look(mut self, look: Look) -> Self {
        self.look = look;
        self
    }

    pub fn look(&self) -> &Look {
        &self.look
    }

    pub fn set_look(&mut self, look: Look, cx: &mut Context<Self>) {
        self.look = look;
        cx.notify();
    }

    pub fn text(&self) -> &str {
        &self.content
    }

    pub fn set_masked(&mut self, masked: bool, cx: &mut Context<Self>) {
        self.look.masked = masked;
        cx.notify();
    }

    pub fn set_placeholder(
        &mut self,
        placeholder: impl Into<SharedString>,
        cx: &mut Context<Self>,
    ) {
        self.placeholder = placeholder.into();
        cx.notify();
    }

    pub fn clear(&mut self, window: &Window, cx: &mut Context<Self>) {
        self.reset(window, cx);
    }

    /// Replaces the text, caret at the end.
    pub fn set_caret(&mut self, at: usize, cx: &mut Context<Self>) {
        let at = at.min(self.content.len());
        self.selected_range = at..at;
        self.reveal = true;
        cx.notify();
    }

    pub fn set_text(&mut self, text: &str, cx: &mut Context<Self>) {
        self.forget();
        self.content = text.to_string().into();
        self.selected_range = self.content.len()..self.content.len();
        self.marked_range = None;
        self.reveal = true;
        cx.notify();
    }

    fn cancel(&mut self, _: &Cancel, _: &mut Window, cx: &mut Context<Self>) {
        if self.nav || self.chip {
            return cx.emit(Nav::Close);
        }
        cx.emit(CancelInput);
    }

    /// Up, Down, Enter, Tab and Escape go to an open autocomplete.
    pub fn set_nav(&mut self, on: bool) {
        self.nav = on;
    }

    /// Tab and Escape (not Enter or the arrows) go to a suggestion chip.
    /// Colour markup previews as it will send; `Some(true)` when a leading colour command
    /// is Vector's own here.
    pub fn set_colors(&mut self, colors: Option<bool>) -> bool {
        std::mem::replace(&mut self.colors, colors) != colors
    }

    pub fn set_chip(&mut self, on: bool) {
        self.chip = on;
    }

    pub fn has_selection(&self) -> bool {
        !self.selected_range.is_empty()
    }

    /// Where a content offset is drawn, in window coordinates, as of the last paint.
    pub fn x_at(&self, offset: usize) -> Option<Pixels> {
        let (bounds, layout) = (self.last_bounds?, self.last_layout.as_ref()?);
        if layout.text != self.content {
            return None;
        }
        Some(bounds.origin.x + layout.position(offset).x)
    }

    /// The caret's byte offset.
    pub fn caret(&self) -> usize {
        self.cursor_offset()
    }

    /// Replaces a byte range, the caret after what went in.
    pub fn replace(
        &mut self,
        range: Range<usize>,
        text: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let range = range.start.min(self.content.len())..range.end.min(self.content.len());
        self.selected_range = range;
        self.marked_range = None;
        self.replace_text_in_range(None, text, window, cx);
    }

    /// A content offset as an offset into what is drawn.
    fn to_display(&self, offset: usize) -> usize {
        if !self.look.masked {
            let mut shift = 0isize;
            for sub in self.subs().iter() {
                if offset <= sub.content.start {
                    break;
                }
                if offset < sub.content.end {
                    return sub.display.start;
                }
                shift = sub.display.end as isize - sub.content.end as isize;
            }
            return (offset as isize + shift) as usize;
        }
        self.content[..offset.min(self.content.len())]
            .graphemes(true)
            .count()
            * BULLET.len()
    }

    /// An offset into what is drawn as a content offset.
    fn from_display(&self, offset: usize) -> usize {
        if !self.look.masked {
            let mut shift = 0isize;
            for sub in self.subs().iter() {
                if offset <= sub.display.start {
                    break;
                }
                if offset < sub.display.end {
                    return if offset - sub.display.start > sub.display.len() / 2 {
                        sub.content.end
                    } else {
                        sub.content.start
                    };
                }
                shift = sub.content.end as isize - sub.display.end as isize;
            }
            return (offset as isize + shift) as usize;
        }
        let n = offset / BULLET.len();
        self.content
            .grapheme_indices(true)
            .nth(n)
            .map(|(i, _)| i)
            .unwrap_or(self.content.len())
    }

    /// Enter, as if pressed.
    pub fn submit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.send(&Send, window, cx);
    }

    fn send(&mut self, _: &Send, window: &mut Window, cx: &mut Context<Self>) {
        // Enter mid-composition commits the IME text rather than sending it.
        if self.marked_range.is_some() {
            return;
        }
        if self.nav {
            return cx.emit(Nav::Pick);
        }
        if self.decorated.is_some()
            && self.multiline.is_some()
            && crate::backend::device::get("send_on_enter").as_deref() == Some("false")
        {
            return self.replace_text_in_range(None, "\n", window, cx);
        }
        self.send_now(&SendNow, window, cx);
    }

    /// Ctrl/Cmd + Enter sends whatever Send on Enter says.
    fn send_now(&mut self, _: &SendNow, window: &mut Window, cx: &mut Context<Self>) {
        if self.marked_range.is_some() {
            return;
        }
        if self.nav {
            return cx.emit(Nav::Pick);
        }
        let text = self.content.trim().to_string();
        if text.is_empty() && !self.submit_empty {
            return;
        }
        if self.clear_on_submit {
            self.reset(window, cx);
        }
        cx.emit(SendText(text));
    }
}

pub fn bind_keys(cx: &mut App) {
    cx.bind_keys([
        KeyBinding::new("backspace", Backspace, None),
        KeyBinding::new("delete", Delete, None),
        KeyBinding::new("left", Left, None),
        KeyBinding::new("right", Right, None),
        KeyBinding::new("shift-left", SelectLeft, None),
        KeyBinding::new("shift-right", SelectRight, None),
        KeyBinding::new(
            if cfg!(target_os = "macos") {
                "cmd-a"
            } else {
                "ctrl-a"
            },
            SelectAll,
            None,
        ),
        KeyBinding::new(
            if cfg!(target_os = "macos") {
                "cmd-v"
            } else {
                "ctrl-v"
            },
            Paste,
            None,
        ),
        KeyBinding::new(
            if cfg!(target_os = "macos") {
                "cmd-c"
            } else {
                "ctrl-c"
            },
            Copy,
            None,
        ),
        KeyBinding::new(
            if cfg!(target_os = "macos") {
                "cmd-x"
            } else {
                "ctrl-x"
            },
            Cut,
            None,
        ),
        KeyBinding::new("home", Home, None),
        KeyBinding::new("end", End, None),
        KeyBinding::new("ctrl-cmd-space", ShowCharacterPalette, None),
        KeyBinding::new("enter", Send, Some("TextInput")),
        KeyBinding::new("shift-enter", Newline, Some("TextInput")),
        KeyBinding::new("up", Up, Some("TextInput")),
        KeyBinding::new("down", Down, Some("TextInput")),
        KeyBinding::new("shift-up", SelectUp, Some("TextInput")),
        KeyBinding::new("shift-down", SelectDown, Some("TextInput")),
        KeyBinding::new("escape", Cancel, Some("TextInput")),
        KeyBinding::new("tab", Tab, Some("TextInput")),
        KeyBinding::new("shift-home", SelectHome, None),
        KeyBinding::new("shift-end", SelectEnd, None),
    ]);
    // macOS moves by word on Option and to the ends on Cmd, where Ctrl B and E move the caret.
    macro_rules! os {
        ($mac:literal, $other:literal) => {
            if cfg!(target_os = "macos") { $mac } else { $other }
        };
    }
    cx.bind_keys([
        KeyBinding::new(os!("cmd-z", "ctrl-z"), Undo, None),
        KeyBinding::new(os!("cmd-shift-z", "ctrl-shift-z"), Redo, None),
        KeyBinding::new(os!("alt-left", "ctrl-left"), WordLeft, None),
        KeyBinding::new(os!("alt-right", "ctrl-right"), WordRight, None),
        KeyBinding::new(os!("alt-shift-left", "ctrl-shift-left"), SelectWordLeft, None),
        KeyBinding::new(os!("alt-shift-right", "ctrl-shift-right"), SelectWordRight, None),
        KeyBinding::new(os!("alt-backspace", "ctrl-backspace"), DeleteWordLeft, None),
        KeyBinding::new(os!("alt-delete", "ctrl-delete"), DeleteWordRight, None),
        KeyBinding::new(os!("cmd-b", "ctrl-b"), Bold, Some("TextInput")),
        KeyBinding::new(os!("cmd-i", "ctrl-i"), Italic, Some("TextInput")),
        KeyBinding::new(os!("cmd-e", "ctrl-e"), Code, Some("TextInput")),
        KeyBinding::new(os!("cmd-shift-x", "ctrl-shift-x"), Strike, Some("TextInput")),
        KeyBinding::new(os!("cmd-enter", "ctrl-enter"), SendNow, Some("TextInput")),
    ]);
    if cfg!(target_os = "macos") {
        cx.bind_keys([
            KeyBinding::new("cmd-left", Home, None),
            KeyBinding::new("cmd-right", End, None),
            KeyBinding::new("cmd-shift-left", SelectHome, None),
            KeyBinding::new("cmd-shift-right", SelectEnd, None),
        ]);
    } else {
        cx.bind_keys([KeyBinding::new("ctrl-y", Redo, None)]);
    }
}

#[cfg(test)]
mod npub_tests {
    use super::npub_mentions;

    #[test]
    fn pasted_npub_mentions_outside_code() {
        let n = "npub1v84qsqlcs56j8dmh6s22eccnpn2d87fdutxhl7rftseh67wzamhqfv9ft6";
        let text = format!("hi @{n} and `@{n}` and @npub1bad");
        let found = npub_mentions(&text);
        assert_eq!(found.len(), 1);
        assert_eq!(&text[found[0].clone()], format!("@{n}"));
    }
}
