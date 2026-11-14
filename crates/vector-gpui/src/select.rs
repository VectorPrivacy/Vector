//! Selecting message text, across messages, as a page selects it. Each paragraph a message draws
//! says where it was laid out; a press, drag or multi-click picks a span over them, and a copy
//! writes the text between in reading order.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::ops::Range;
use std::rc::Rc;

use gpui::{Bounds, Pixels, Point, Window};

use crate::inline::{Atom, Laid};

/// A paragraph's place in reading order: its message's time, then the message, then where in
/// the message's blocks it sits (each step one more than its index, so a parent sorts first).
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Key {
    pub at: u64,
    /// The message id's hash: built per paragraph per draw, so nothing is allocated.
    pub msg: u64,
    pub path: [u16; 4],
}

fn msg_hash(id: &str) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    id.hash(&mut h);
    h.finish()
}

impl Key {
    /// From a block's element key, `<message id>-<i>[-<j>…]`; None for any other key.
    #[cfg(test)]
    fn of_block(at: u64, msg: &str, block_key: &str) -> Option<Self> {
        let mut path = [0u16; 4];
        let rest = block_key.strip_prefix(msg)?.strip_prefix('-')?;
        for (slot, part) in path.iter_mut().zip(rest.split('-')) {
            *slot = part.parse::<u16>().ok()?.saturating_add(1);
        }
        Some(Key { at, msg: msg_hash(msg), path })
    }
}

thread_local! {
    static SCOPE: RefCell<Option<(u64, u64, usize)>> = const { RefCell::new(None) };
}

/// While alive, paragraphs built for this message's body are selectable.
pub struct Scope;

impl Scope {
    pub fn enter(at: u64, msg: &str) -> Scope {
        SCOPE.with(|s| *s.borrow_mut() = Some((at, msg_hash(msg), msg.len())));
        Scope
    }
}

impl Drop for Scope {
    fn drop(&mut self) {
        SCOPE.with(|s| *s.borrow_mut() = None);
    }
}

/// The key a body paragraph built now is selected by.
pub fn key_for(block_key: &str) -> Option<Key> {
    SCOPE.with(|s| {
        let s = s.borrow();
        let &(at, msg, len) = s.as_ref()?;
        let rest = block_key.get(len..)?.strip_prefix('-')?;
        if msg_hash(&block_key[..len]) != msg {
            return None;
        }
        let mut path = [0u16; 4];
        for (slot, part) in path.iter_mut().zip(rest.split('-')) {
            *slot = part.parse::<u16>().ok()?.saturating_add(1);
        }
        Some(Key { at, msg, path })
    })
}

struct Placed {
    atoms: Rc<[Atom]>,
    laid: Rc<RefCell<Laid>>,
    bounds: Bounds<Pixels>,
    /// The draw this paragraph was last laid out in.
    pass: u64,
}

#[derive(Default)]
struct State {
    placed: BTreeMap<Key, Placed>,
    /// Bumped each time the timeline draws: paragraphs laid out since are the ones on screen.
    pass: u64,
    anchor: Option<(Key, usize)>,
    head: Option<(Key, usize)>,
    dragging: bool,
}

thread_local! {
    static STATE: RefCell<State> = RefCell::new(State::default());
}

/// The timeline is drawing again: what it lays out now is what's on screen. Paragraphs that
/// left the screen are let go, unless the selection still spans them.
pub fn next_pass() {
    STATE.with(|s| {
        let mut s = s.borrow_mut();
        let span = ordered(&s).map(|(lo, hi)| (lo.0, hi.0));
        let pass = s.pass;
        s.placed.retain(|k, p| p.pass == pass || span.as_ref().is_some_and(|(lo, hi)| k >= lo && k <= hi));
        s.pass += 1;
    });
}

/// A paragraph was laid out at `bounds`.
pub fn place(key: &Key, atoms: &Rc<[Atom]>, laid: &Rc<RefCell<Laid>>, bounds: Bounds<Pixels>) {
    STATE.with(|s| {
        let mut s = s.borrow_mut();
        let pass = s.pass;
        match s.placed.get_mut(key) {
            Some(p) => {
                if !Rc::ptr_eq(&p.atoms, atoms) {
                    p.atoms = atoms.clone();
                }
                if !Rc::ptr_eq(&p.laid, laid) {
                    p.laid = laid.clone();
                }
                p.bounds = bounds;
                p.pass = pass;
            }
            None => {
                s.placed.insert(key.clone(), Placed { atoms: atoms.clone(), laid: laid.clone(), bounds, pass });
            }
        }
    });
}

/// Forget every paragraph and the selection: another chat opened.
pub fn reset() {
    STATE.with(|s| *s.borrow_mut() = State::default());
}

pub fn dragging() -> bool {
    STATE.with(|s| s.borrow().dragging)
}

/// The span selected in this paragraph, if any of it is.
pub fn range_in(key: &Key, len: impl FnOnce() -> usize) -> Option<Range<usize>> {
    STATE.with(|s| {
        let s = s.borrow();
        let (lo, hi) = ordered(&s)?;
        if *key < lo.0 || *key > hi.0 {
            return None;
        }
        let start = if *key == lo.0 { lo.1 } else { 0 };
        let end = if *key == hi.0 { hi.1 } else { len() };
        (start < end).then_some(start..end)
    })
}

fn ordered(s: &State) -> Option<((Key, usize), (Key, usize))> {
    let (a, h) = (s.anchor.clone()?, s.head.clone()?);
    if a == h {
        return None;
    }
    Some(if (&a.0, a.1) <= (&h.0, h.1) { (a, h) } else { (h, a) })
}

/// The on-screen paragraph under `pos`, or failing that the nearest one.
fn under(s: &State, pos: Point<Pixels>, nearest: bool) -> Option<Key> {
    let on_screen = s.placed.iter().filter(|(_, p)| p.pass == s.pass);
    if let Some((k, _)) = on_screen.clone().find(|(_, p)| p.bounds.contains(&pos)) {
        return Some(k.clone());
    }
    if !nearest {
        return None;
    }
    let dist = |b: &Bounds<Pixels>| {
        let dy = (f32::from(b.top()) - f32::from(pos.y)).max(f32::from(pos.y) - f32::from(b.bottom())).max(0.);
        let dx = (f32::from(b.left()) - f32::from(pos.x)).max(f32::from(pos.x) - f32::from(b.right())).max(0.);
        dy * 1000. + dx
    };
    on_screen.min_by(|a, b| dist(&a.1.bounds).total_cmp(&dist(&b.1.bounds))).map(|(k, _)| k.clone())
}

fn offset_at(p: &Placed, pos: Point<Pixels>, window: &Window) -> usize {
    crate::inline::hit(&p.atoms, &p.laid.borrow(), pos - p.bounds.origin, window)
}

/// A press at `pos`, the `clicks`th of a run. Some(changed) when the timeline should redraw:
/// a press starts a selection at the nearest text (a word on a double, the paragraph on a
/// triple), dropping any before it.
pub fn press(pos: Point<Pixels>, clicks: usize, window: &Window) -> Option<bool> {
    STATE.with(|s| {
        let mut s = s.borrow_mut();
        let Some(key) = under(&s, pos, true) else {
            let had = ordered(&s).is_some();
            s.anchor = None;
            s.head = None;
            s.dragging = false;
            return had.then_some(true);
        };
        let p = &s.placed[&key];
        let at = offset_at(p, pos, window);
        let (start, end) = match clicks {
            0 | 1 => (at, at),
            2 => {
                let text = crate::inline::plain(&p.atoms);
                let r = word_around(&text, at);
                (r.start, r.end)
            }
            _ => (0, crate::inline::plain(&p.atoms).len()),
        };
        let had = ordered(&s).is_some();
        s.anchor = Some((key.clone(), start));
        s.head = Some((key, end));
        s.dragging = clicks <= 1;
        Some(had || start != end)
    })
}

/// The pointer moved with the button held: the selection's free end follows it. True when it
/// changed.
pub fn drag(pos: Point<Pixels>, window: &Window) -> bool {
    STATE.with(|s| {
        let mut s = s.borrow_mut();
        if !s.dragging {
            return false;
        }
        let Some(key) = under(&s, pos, true) else { return false };
        let at = offset_at(&s.placed[&key], pos, window);
        let head = Some((key, at));
        if s.head == head {
            return false;
        }
        s.head = head;
        true
    })
}

/// For devctl: the paragraphs on screen and the selection's ends.
#[cfg(feature = "devctl")]
pub fn debug() -> serde_json::Value {
    STATE.with(|s| {
        let s = s.borrow();
        let on: Vec<_> = s
            .placed
            .iter()
            .filter(|(_, p)| p.pass == s.pass)
            .map(|(k, p)| serde_json::json!({ "msg": k.msg, "path": k.path, "bounds": format!("{:?}", p.bounds) }))
            .collect();
        serde_json::json!({ "pass": s.pass, "placed": s.placed.len(), "on_screen": on, "anchor": format!("{:?}", s.anchor), "head": format!("{:?}", s.head), "text": text() })
    })
}

pub fn release() {
    STATE.with(|s| s.borrow_mut().dragging = false);
}

/// The selected text in reading order: paragraphs of one message a line apart, messages a blank
/// line apart. Messages never drawn while the selection spanned them are left out.
pub fn text() -> Option<String> {
    STATE.with(|s| {
        let s = s.borrow();
        let (lo, hi) = ordered(&s)?;
        let mut out = String::new();
        let mut last: Option<u64> = None;
        for (key, p) in s.placed.range(lo.0.clone()..=hi.0.clone()) {
            let text = crate::inline::plain(&p.atoms);
            let start = if *key == lo.0 { lo.1.min(text.len()) } else { 0 };
            let end = if *key == hi.0 { hi.1.min(text.len()) } else { text.len() };
            let (Some(start), Some(end)) = (floor_char(&text, start), floor_char(&text, end)) else { continue };
            // The layout's break hints in long URLs are invisible and don't belong in a copy.
            let piece: String = text[start..end.max(start)].chars().filter(|c| !matches!(c, '\u{2060}' | '\u{200B}' | '\u{00AD}')).collect();
            // An empty end contributes nothing; an empty line inside (a blank code line) keeps
            // its place.
            if piece.is_empty() && (*key == lo.0 || *key == hi.0) {
                continue;
            }
            if let Some(prev) = last {
                out.push_str(if prev == key.msg { "\n" } else { "\n\n" });
            }
            out.push_str(&piece);
            last = Some(key.msg);
        }
        (!out.is_empty()).then_some(out)
    })
}

fn floor_char(text: &str, mut at: usize) -> Option<usize> {
    while at > 0 && !text.is_char_boundary(at) {
        at -= 1;
    }
    Some(at)
}

/// The run under `offset`: word characters, punctuation or spaces, as a double-click picks.
fn word_around(text: &str, offset: usize) -> Range<usize> {
    let offset = floor_char(text, offset.min(text.len())).unwrap_or(0);
    let class = |c: char| if c.is_alphanumeric() || c == '_' { 0 } else if c.is_whitespace() { 1 } else { 2 };
    let here = text[offset..]
        .chars()
        .next()
        .filter(|&c| c != '\n')
        .or_else(|| text[..offset].chars().next_back())
        .map(class);
    let Some(want) = here else { return offset..offset };
    let start = text[..offset]
        .char_indices()
        .rev()
        .take_while(|&(_, c)| c != '\n' && class(c) == want)
        .last()
        .map_or(offset, |(i, _)| i);
    let end = text[offset..]
        .char_indices()
        .find(|&(_, c)| c == '\n' || class(c) != want)
        .map_or(text.len(), |(i, _)| offset + i);
    start..end
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_sort_in_reading_order() {
        let k = |at, m, b| Key::of_block(at, m, b).unwrap();
        let (a, b, b0, c) = (k(5, "m1", "m1-0"), k(5, "m1", "m1-1"), k(5, "m1", "m1-1-0"), k(6, "m0", "m0-0"));
        assert!(a < b && b < b0 && b0 < c);
        assert!(k(5, "m1", "m1-2") < k(5, "m1", "m1-10"));
        assert!(Key::of_block(5, "m1", "embed-m1-0").is_none());
        assert!(Key::of_block(5, "m1", "m1-copy").is_none());
        let _s = Scope::enter(5, "m1");
        assert_eq!(key_for("m1-1-0"), Some(b0));
        assert_eq!(key_for("m2-1"), None);
    }

    #[test]
    fn a_double_click_picks_the_word() {
        let t = "hello brave world";
        assert_eq!(&t[word_around(t, 8)], "brave");
        assert_eq!(&t[word_around(t, 5)], " ");
        assert_eq!(&t[word_around(t, t.len())], "world");
    }
}
