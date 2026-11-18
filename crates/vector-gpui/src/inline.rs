//! Inline formatting as CSS lays it out: runs at mixed sizes and weights, emoji images and
//! boxed spans (inline code, spoilers, mention pills) filled into lines that break after
//! spaces (UAX #14), anywhere inside a word too wide for a line (`overflow-wrap: anywhere`),
//! with baselines aligned and each line as tall as its tallest box.
//!
//! GPUI's own text shapes one size per layout and has no inline boxes, which a chat message
//! needs on every line: `(edited)` at 10px after 15px text, a bordered code chip, a twemoji.

use std::cell::{Cell, RefCell};
use std::ops::Range;
use std::rc::Rc;

use gpui::{AnyElement, App, Bounds, Corners, CursorStyle, Edges, Element, ElementId, Font, FontStyle, FontWeight, GlobalElementId, Hitbox, HitboxBehavior, Hsla, InspectorElementId, IntoElement, LayoutId, MouseButton, MouseDownEvent, MouseUpEvent, Pixels, ShapedLine, SharedString, Size, StrikethroughStyle, Style, Styled, TextAlign, TextRun, Window, fill, outline, point, px, size, div, InteractiveElement, StatefulInteractiveElement, ParentElement, prelude::FluentBuilder};
use crate::pic::img;
use unicode_segmentation::UnicodeSegmentation;

use crate::theme::*;

/// How a run of text looks.
#[derive(Clone, PartialEq)]
pub struct Look {
    pub family: SharedString,
    pub size: f32,
    pub line_height: f32,
    pub weight: FontWeight,
    pub italic: bool,
    pub color: Hsla,
    pub strike: bool,
    pub underline: bool,
    /// A highlight behind the glyphs.
    pub bg: Option<Hsla>,
    /// Raised off the line's baseline (`<sup>`), or lowered when negative (`<sub>`).
    pub rise: f32,
}

impl Look {
    pub fn new(family: &'static str, size: f32, line_height: f32, color: Hsla) -> Self {
        Self {
            family: family.into(),
            size,
            line_height,
            weight: FontWeight::NORMAL,
            italic: false,
            color,
            strike: false,
            underline: false,
            bg: None,
            rise: 0.,
        }
    }

    fn font(&self) -> Font {
        Font {
            family: self.family.clone(),
            features: Default::default(),
            fallbacks: None,
            weight: self.weight,
            style: if self.italic {
                FontStyle::Italic
            } else {
                FontStyle::Normal
            },
        }
    }

    fn run(&self, len: usize, color: Hsla) -> TextRun {
        TextRun {
            len,
            font: self.font(),
            color,
            background_color: None,
            underline: self.underline.then(|| gpui::UnderlineStyle {
                thickness: px(1.),
                color: Some(color),
                wavy: false,
            }),
            strikethrough: self.strike.then(|| StrikethroughStyle {
                thickness: px(1.),
                color: Some(color),
            }),
        }
    }
}

const RING: f32 = 16.;
const RING_GAP: f32 = 6.;
/// `vertical-align: middle` for a 16px box beside 15px text.
const RING_DROP: f32 = 4.5;

/// `Kind::Action` for an emoji that couldn't load; the reason follows.
pub const EMOJI_MISSING: &str = "emoji-missing:";

/// What a run is, beyond its look: what clicking it does and the box drawn behind it.
#[derive(Clone, PartialEq)]
pub enum Kind {
    Plain,
    Link(SharedString),
    /// A link whose text isn't its destination: the destination shows on hover.
    LinkTip(SharedString, SharedString),
    /// `code`: a bordered chip, copied on click.
    Code,
    /// `||spoiler||`, hidden until clicked; the id is its index in the message.
    Spoiler {
        id: u32,
        revealed: bool,
    },
    /// A profile pill.
    Mention(SharedString),
    /// `@everyone`, or a Role in its colour: a chip that goes nowhere.
    Tag(Option<u32>),
    /// `.vt-time`: a time in the reader's zone, never broken across lines; its hover hint.
    Time(SharedString),
    /// Quiet text that does something when clicked, brightening under the pointer.
    Action(SharedString),
}

/// What hovering an atom says.
#[derive(Clone, PartialEq, Debug)]
pub enum Tip {
    Text(SharedString),
    /// `.dmsg-selfdestruct`: the countdown to a NIP-40 expiry, read when shown.
    Dissolve(i64),
}

impl Tip {
    fn text(&self) -> String {
        match self {
            Tip::Text(t) => t.to_string(),
            Tip::Dissolve(at) => {
                let left = at - chrono::Utc::now().timestamp();
                if left > 0 {
                    crate::t::msg_dissolves_in(crate::timeline::countdown(left as u64)).to_string()
                } else {
                    crate::t::msg_dissolving().into()
                }
            }
        }
    }
}

impl Kind {
    /// Horizontal padding plus border, each side.
    fn pad(&self) -> f32 {
        match self {
            Kind::Code => 7.,
            Kind::Spoiler { .. } => 4.,
            Kind::Mention(_) | Kind::Tag(_) | Kind::Time(_) => 4.,
            _ => 0.,
        }
    }

    fn target(&self, text: &str) -> Option<Target> {
        match self {
            Kind::Plain | Kind::Time(_) | Kind::Tag(_) => None,
            Kind::Link(url) | Kind::LinkTip(url, _) => Some(Target::Link(url.clone())),
            Kind::Code => Some(Target::Code(text.to_string().into())),
            Kind::Spoiler {
                id,
                revealed: false,
            } => Some(Target::Spoiler(*id)),
            Kind::Spoiler { .. } => None,
            Kind::Mention(npub) => Some(Target::Mention(npub.clone())),
            Kind::Action(a) => Some(Target::Action(a.clone())),
        }
    }
}

#[derive(Clone)]
pub enum Atom {
    Text {
        text: SharedString,
        look: Look,
        kind: Kind,
    },
    /// A square image (a twemoji or custom emoji), `vertical-align` below the baseline by `drop`.
    Image {
        src: gpui::ImageSource,
        size: f32,
        drop: f32,
        /// Shown at 40%, whole on hover, with this hint.
        tip: Option<Tip>,
        /// What a copy of the selection writes in its place: the emoji or its `:shortcode:`.
        alt: SharedString,
    },
    /// Fixed space that never breaks from what follows (an inline box's margin).
    Gap(f32),
    /// A linked picture's fetch (`.inline-image-loading`): 6px off the text, a 16px ring.
    Ring(Option<f32>),
    /// A forced line break.
    Break,
    /// `.mention.go-link`: a Vector link as the place it names, one unbreakable box.
    Pill(Rc<Pill>),
}

pub struct Pill {
    pub parts: Vec<Part>,
    pub look: Look,
    pub tip: Option<SharedString>,
    pub target: Target,
}

pub enum Part {
    Icon(&'static str),
    Avatar(gpui::ImageSource),
    Text { text: SharedString, alpha: f32, strike: bool },
}

const PILL_PAD: f32 = 4.;
const PILL_GAP: f32 = 3.;

impl Pill {
    fn width(&self, window: &Window) -> f32 {
        let em = self.look.size;
        let parts: f32 = self
            .parts
            .iter()
            .map(|p| match p {
                Part::Icon(_) => 0.95 * em,
                Part::Avatar(_) => 1.15 * em + 1.,
                Part::Text { text, .. } => f32::from(shape(text, &self.look, self.look.color, window).width),
            })
            .sum();
        parts + PILL_GAP * self.parts.len().saturating_sub(1) as f32 + 2. * PILL_PAD
    }

    fn element(&self, id: ElementId, h: f32) -> AnyElement {
        let em = self.look.size;
        let tip = self.tip.clone();
        div()
            .id(id)
            .h(px(h))
            .px(px(PILL_PAD))
            .flex()
            .items_center()
            .gap(px(PILL_GAP))
            .rounded(px(4.))
            .bg(white_a(0.08))
            .hover(|s| s.bg(white_a(0.12)))
            .when_some(tip, |d, tip| d.tooltip(move |w, cx| crate::kit::tooltip(tip.clone())(w, cx)))
            .font_family(self.look.family.clone())
            .text_size(px(em))
            .line_height(px(self.look.line_height))
            .font_weight(self.look.weight)
            .text_color(self.look.color)
            .whitespace_nowrap()
            .children(self.parts.iter().map(|p| match p {
                Part::Icon(path) => gpui::svg()
                    .path(*path)
                    .size(px(0.95 * em))
                    .flex_shrink_0()
                    .text_color(self.look.color)
                    .into_any_element(),
                Part::Avatar(src) => gpui::StyledImage::object_fit(img(src.clone()), gpui::ObjectFit::Cover)
                    .size(px(1.15 * em))
                    .mr(px(1.))
                    .flex_shrink_0()
                    .rounded_full()
                    .into_any_element(),
                Part::Text { text, alpha, strike } => div()
                    .opacity(*alpha)
                    .when(*strike, |d| d.line_through())
                    .child(text.clone())
                    .into_any_element(),
            }))
            .into_any_element()
    }
}

/// What a click landed on.
#[derive(Clone, Debug)]
pub enum Target {
    Link(SharedString),
    Code(SharedString),
    Spoiler(u32),
    Mention(SharedString),
    Action(SharedString),
}

pub type OnClick = Rc<dyn Fn(Target, &mut Window, &mut App)>;

/// A paragraph of atoms; `base` is the block's own font, whose strut every line keeps.
pub struct Inline {
    id: Option<ElementId>,
    atoms: Rc<[Atom]>,
    base: Look,
    on_click: Option<OnClick>,
    clock: f32,
    /// One line, cut with `…` (`white-space: nowrap; text-overflow: ellipsis`).
    ellipsis: bool,
    /// Told whether the one line was cut, as laid out.
    clipped: Option<Rc<std::cell::Cell<bool>>>,
    /// A message's paragraph, which a drag can select.
    select: Option<crate::select::Key>,
    /// Never wraps, however narrow its box (`white-space: pre`).
    nowrap: bool,
    /// Paragraphs start left to right even in a right-to-left UI.
    ltr: bool,
    /// Each paragraph sits at its own start edge rather than the UI's (`dir=auto`).
    dir_auto: bool,
    /// Its pictures hold their first frame until their row is hovered (`data-motion="hover"`).
    held: bool,
}

pub fn inline(atoms: impl Into<Rc<[Atom]>>, base: Look) -> Inline {
    Inline {
        id: None,
        atoms: atoms.into(),
        base,
        on_click: None,
        clock: 0.,
        ellipsis: false,
        clipped: None,
        select: None,
        nowrap: false,
        ltr: false,
        dir_auto: false,
        held: false,
    }
}

impl Inline {
    pub fn id(mut self, id: impl Into<ElementId>) -> Self {
        self.id = Some(id.into());
        self
    }

    pub fn on_click(mut self, f: OnClick) -> Self {
        self.on_click = Some(f);
        self
    }

    pub fn single_line(mut self) -> Self {
        self.ellipsis = true;
        self
    }

    /// A one-line cut repaints the next frame when it changes.
    pub fn report_clip(mut self, cell: Rc<std::cell::Cell<bool>>) -> Self {
        self.clipped = Some(cell);
        self
    }

    /// One line however wide: a code line, clipped by its box rather than wrapped.
    pub fn nowrap(mut self) -> Self {
        self.nowrap = true;
        self
    }

    /// Code: its lines run left to right whatever the UI's direction.
    pub fn ltr(mut self) -> Self {
        self.ltr = true;
        self
    }

    /// Message text: a paragraph that reads left to right stays left in a right-to-left UI,
    /// where a label sits against the UI's start edge whatever its script.
    pub fn dir_auto(mut self) -> Self {
        self.dir_auto = true;
        self
    }

    /// Animated pictures in it move only while the nearest motion host is hovered: names,
    /// statuses and previews in a list row, as `motion.js` holds them.
    pub fn held(mut self) -> Self {
        self.held = true;
        self
    }

    /// Selectable, as this paragraph of a message.
    pub fn selectable(mut self, key: crate::select::Key) -> Self {
        self.select = Some(key);
        self
    }

    /// Seconds, for the hidden spoilers' shimmer.
    pub fn clock(mut self, t: f32) -> Self {
        self.clock = t;
        self
    }
}

/// A piece of one atom placed on a line.
struct Placed {
    atom: usize,
    /// Byte range of a text atom.
    range: Range<usize>,
    x: f32,
    width: f32,
    /// Box padding before and after the glyphs (a chip's first and last piece).
    lead: f32,
    tail: f32,
    /// Brackets in a right-to-left run with no letter to tell the shaper so: drawn mirrored.
    mirror: bool,
}

struct Line {
    top: f32,
    height: f32,
    baseline: f32,
    /// How far the glyphs' own baseline sits above `baseline`: GPUI reports descent negative.
    sink: f32,
    items: Vec<Placed>,
    /// Set against the right edge: a right-to-left paragraph's line, or a label's in a
    /// mirrored UI.
    right: bool,
}

#[derive(Default)]
pub struct Laid {
    width: Option<f32>,
    lines: Vec<Line>,
    /// The width the right-set lines currently end at.
    flush: f32,
    /// Where a cut line's `…` goes, in which atom's look.
    ellipsis: Option<(f32, usize)>,
    size: Size<Pixels>,
}

/// A breakable unit: a span of one atom that ends where a line may break.
struct Seg {
    atom: usize,
    range: Range<usize>,
    width: f32,
    /// Width of the trailing spaces, which hang past the line's end.
    hang: f32,
    lead: f32,
    tail: f32,
    /// A line may break after this segment.
    brk: bool,
    /// Forced line break after.
    hard: bool,
}

struct Metrics {
    ascent: f32,
    descent: f32,
}

fn metrics(look: &Look, window: &Window) -> Metrics {
    let ts = window.text_system();
    let id = ts.resolve_font(&look.font());
    let s = px(look.size);
    Metrics {
        ascent: ts.ascent(id, s).into(),
        // CoreText and DirectWrite report it below the baseline as negative.
        descent: f32::from(ts.descent(id, s)).abs(),
    }
}

fn shape(text: &str, look: &Look, color: Hsla, window: &Window) -> ShapedLine {
    let text: SharedString = text.to_string().into();
    window.text_system().shape_line(
        text.clone(),
        px(look.size),
        &[look.run(text.len(), color)],
        None,
    )
}

fn segments(atoms: &[Atom], window: &Window) -> Vec<Seg> {
    let mut segs = Vec::new();
    for (i, atom) in atoms.iter().enumerate() {
        match atom {
            Atom::Break => {
                if let Some(last) = segs.last_mut() {
                    let last: &mut Seg = last;
                    last.hard = true;
                    last.brk = true;
                } else {
                    segs.push(Seg {
                        atom: i,
                        range: 0..0,
                        width: 0.,
                        hang: 0.,
                        lead: 0.,
                        tail: 0.,
                        brk: true,
                        hard: true,
                    });
                }
            }
            Atom::Gap(w) => {
                segs.push(Seg {
                    atom: i,
                    range: 0..0,
                    width: *w,
                    hang: 0.,
                    lead: 0.,
                    tail: 0.,
                    brk: false,
                    hard: false,
                });
            }
            Atom::Image { size, .. } => {
                // A replaced element breaks on either side.
                if let Some(last) = segs.last_mut() {
                    last.brk = true;
                }
                segs.push(Seg {
                    atom: i,
                    range: 0..0,
                    width: *size,
                    hang: 0.,
                    lead: 0.,
                    tail: 0.,
                    brk: true,
                    hard: false,
                });
            }
            Atom::Pill(p) => {
                if let Some(last) = segs.last_mut() {
                    last.brk = true;
                }
                segs.push(Seg {
                    atom: i,
                    range: 0..0,
                    width: p.width(window),
                    hang: 0.,
                    lead: 0.,
                    tail: 0.,
                    brk: true,
                    hard: false,
                });
            }
            Atom::Ring(_) => {
                if let Some(last) = segs.last_mut() {
                    last.brk = true;
                }
                segs.push(Seg {
                    atom: i,
                    range: 0..0,
                    width: RING + RING_GAP,
                    hang: 0.,
                    lead: 0.,
                    tail: 0.,
                    brk: true,
                    hard: false,
                });
            }
            Atom::Text { text, look, kind } => {
                if text.is_empty() {
                    continue;
                }
                let line = shape(text, look, look.color, window);
                // The shaper lays right-to-left runs out visually, so offsets into the whole
                // line don't measure a piece of one: such text is measured piece by piece.
                let rtl = has_rtl(text);
                let pad = kind.pad();
                let mut start = 0;
                let first = segs.len();
                let ends: Vec<usize> = if matches!(kind, Kind::Time(_)) {
                    vec![text.len()]
                } else {
                    unicode_linebreak::linebreaks(text).map(|(e, _)| e).collect()
                };
                for end in ends {
                    let piece = &text[start..end];
                    let trimmed = piece.trim_end();
                    let (full, body) = if rtl {
                        let full = f32::from(shape(piece, look, look.color, window).width);
                        let body = if trimmed.len() == piece.len() { full } else { f32::from(shape(trimmed, look, look.color, window).width) };
                        (full, body)
                    } else {
                        (
                            f32::from(line.x_for_index(end) - line.x_for_index(start)),
                            f32::from(line.x_for_index(start + trimmed.len()) - line.x_for_index(start)),
                        )
                    };
                    let at_end = end == text.len();
                    segs.push(Seg {
                        atom: i,
                        range: start..end,
                        width: full,
                        hang: full - body,
                        lead: 0.,
                        tail: 0.,
                        // The text's end breaks only on a space either side of it.
                        brk: !at_end || piece.ends_with(char::is_whitespace),
                        hard: false,
                    });
                    start = end;
                }
                if pad > 0. && segs.len() > first {
                    segs[first].lead = pad;
                    segs.last_mut().unwrap().tail = pad;
                }
            }
        }
    }
    // A text atom that opens with a space lets the previous segment break.
    for k in 1..segs.len() {
        if let Atom::Text { text, .. } = &atoms[segs[k].atom] {
            if segs[k].range.start == 0 && text.starts_with(char::is_whitespace) {
                segs[k - 1].brk = true;
            }
        }
    }
    segs
}

/// `segs[a..b]`, split where it overflows `limit`: graphemes, as `overflow-wrap: anywhere`.
fn split_word(atoms: &[Atom], seg: &Seg, room: f32, window: &Window) -> Option<usize> {
    let Atom::Text { text, look, .. } = &atoms[seg.atom] else {
        return None;
    };
    let line = shape(text, look, look.color, window);
    let base = f32::from(line.x_for_index(seg.range.start));
    let rtl = has_rtl(text);
    let mut cut = None;
    for (off, g) in text[seg.range.clone()].grapheme_indices(true) {
        let end = seg.range.start + off + g.len();
        if end == seg.range.end {
            break;
        }
        let w = if rtl {
            f32::from(shape(&text[seg.range.start..end], look, look.color, window).width)
        } else {
            f32::from(line.x_for_index(end)) - base
        };
        if w + seg.lead <= room {
            cut = Some(end);
        } else {
            break;
        }
    }
    // At least one grapheme per line.
    cut.or_else(|| {
        text[seg.range.clone()]
            .graphemes(true)
            .next()
            .map(|g| seg.range.start + g.len())
            .filter(|&e| e < seg.range.end)
    })
}

fn piece_width(atoms: &[Atom], atom: usize, range: Range<usize>, window: &Window) -> f32 {
    let Atom::Text { text, look, .. } = &atoms[atom] else {
        return 0.;
    };
    if has_rtl(text) {
        return text.get(range).map_or(0., |t| f32::from(shape(t, look, look.color, window).width));
    }
    let line = shape(text, look, look.color, window);
    f32::from(line.x_for_index(range.end) - line.x_for_index(range.start))
}

pub fn has_rtl(text: &str) -> bool {
    use unicode_bidi::{BidiClass, bidi_class};
    text.chars().any(|c| matches!(bidi_class(c), BidiClass::R | BidiClass::AL))
}

fn layout(atoms: &[Atom], base: &Look, width: f32, ltr: bool, window: &Window) -> Laid {
    let mut segs = segments(atoms, window);
    let mut lines: Vec<Vec<Placed>> = vec![Vec::new()];
    let mut x = 0f32;
    let mut i = 0;
    while i < segs.len() {
        // The word: segments up to and including the next break opportunity.
        let mut j = i;
        let mut w = 0f32;
        while j < segs.len() {
            w += segs[j].lead + segs[j].width + segs[j].tail;
            if segs[j].brk {
                break;
            }
            j += 1;
        }
        let j = j.min(segs.len() - 1);
        let visible = w - segs[j].hang;
        if x > 0. && x + visible > width {
            lines.push(Vec::new());
            x = 0.;
        }
        if visible > width && x == 0. {
            // Too wide for any line: place what fits, split the segment that overflows.
            let mut k = i;
            while k <= j {
                let s = &segs[k];
                let sw = s.lead + s.width + s.tail;
                if x + sw - s.hang <= width
                    || matches!(atoms[s.atom], Atom::Image { .. }) && x == 0.
                {
                    place(&mut lines, &mut x, s, atoms);
                    k += 1;
                    continue;
                }
                match split_word(atoms, s, width - x, window) {
                    Some(cut) if x == 0. || cut > s.range.start => {
                        let head_w = piece_width(atoms, s.atom, s.range.start..cut, window);
                        let head = Seg {
                            atom: s.atom,
                            range: s.range.start..cut,
                            width: head_w,
                            hang: 0.,
                            lead: s.lead,
                            tail: 0.,
                            brk: true,
                            hard: false,
                        };
                        place(&mut lines, &mut x, &head, atoms);
                        let rest_w = piece_width(atoms, s.atom, cut..s.range.end, window);
                        let (end, hang, tail, brk, hard) =
                            (s.range.end, s.hang, s.tail, s.brk, s.hard);
                        segs[k] = Seg {
                            atom: s.atom,
                            range: cut..end,
                            width: rest_w,
                            hang,
                            lead: 0.,
                            tail,
                            brk,
                            hard,
                        };
                    }
                    _ if x > 0. => {}
                    _ => {
                        place(&mut lines, &mut x, s, atoms);
                        k += 1;
                        continue;
                    }
                }
                lines.push(Vec::new());
                x = 0.;
            }
        } else {
            for k in i..=j {
                place(&mut lines, &mut x, &segs[k], atoms);
            }
        }
        if segs[j].hard {
            lines.push(Vec::new());
            x = 0.;
        }
        i = j + 1;
    }
    if lines.len() > 1
        && lines.last().is_some_and(|l| l.is_empty())
        && !segs
            .last()
            .is_some_and(|s| s.hard && matches!(atoms[s.atom], Atom::Break))
    {
        lines.pop();
    }
    let bidi = bidi(atoms, ltr || !gpui::layout_rtl());
    let mut flush_right = vec![false; lines.len()];
    if let Some(b) = &bidi {
        for (line, right) in lines.iter_mut().zip(&mut flush_right) {
            *right = line.first().is_some_and(|p| b.para_level(b.starts[p.atom] + p.range.start) == 1);
            reorder(line, atoms, b, window);
        }
    }

    // Vertical: every line holds the block's strut, then each box by its own line-height.
    let strut = metrics(base, window);
    let half = |m: &Metrics, lh: f32| (lh - (m.ascent + m.descent)) / 2.;
    let strut_above = strut.ascent + half(&strut, base.line_height);
    let strut_below = strut.descent + half(&strut, base.line_height);
    let sink = (-strut.descent).max(0.);
    let mut out = Vec::with_capacity(lines.len());
    let mut top = 0f32;
    let mut widest = 0f32;
    for items in lines {
        let (mut above, mut below) = (strut_above, strut_below);
        for p in &items {
            match &atoms[p.atom] {
                Atom::Text { look, .. } if look != base => {
                    let m = metrics(look, window);
                    let h = half(&m, look.line_height);
                    above = above.max(m.ascent + h);
                    below = below.max(m.descent + h);
                }
                Atom::Image { size, drop, .. } => {
                    above = above.max(size - drop + sink);
                    below = below.max(drop - sink);
                }
                Atom::Ring(_) => {
                    above = above.max(RING - RING_DROP);
                    below = below.max(RING_DROP);
                }
                Atom::Pill(pill) => {
                    let m = metrics(&pill.look, window);
                    let h = half(&m, pill.look.line_height) + 1.;
                    above = above.max(m.ascent + h);
                    below = below.max(m.descent + h);
                }
                _ => {}
            }
        }
        let end = items
            .iter()
            .map(|p| p.x + p.lead + p.width + p.tail)
            .fold(0., f32::max);
        widest = widest.max(end);
        let height = above + below;
        out.push(Line {
            top,
            height,
            baseline: top + above,
            sink,
            items,
            right: false,
        });
        top += height;
    }
    // A right-to-left paragraph's lines sit against the block's right edge.
    let block = widest.min(width).ceil();
    for (line, right) in out.iter_mut().zip(flush_right) {
        line.right = right;
        if right {
            let end = line_end(line);
            for p in &mut line.items {
                p.x += block - end;
            }
        }
    }
    Laid {
        width: Some(width),
        lines: out,
        flush: block,
        ellipsis: None,
        size: size(px(block), px(top)),
    }
}

fn line_end(line: &Line) -> f32 {
    line.items.iter().map(|p| p.x + p.lead + p.width + p.tail).fold(0., f32::max)
}

/// Sets every line against the right edge, where a mirrored UI starts. A cut line fills its box.
fn to_start(laid: &mut Laid) {
    if laid.ellipsis.is_some() {
        return;
    }
    let flush = laid.flush;
    for line in laid.lines.iter_mut().filter(|l| !l.right) {
        let shift = flush - line_end(line);
        for p in &mut line.items {
            p.x += shift;
        }
        line.right = true;
    }
}

/// Moves the right-set lines to the edge of the box the paragraph was given, which a stretched
/// layout makes wider than its text.
fn flush_to(laid: &mut Laid, edge: f32) {
    let shift = edge - laid.flush;
    if shift.abs() < 0.5 || !laid.lines.iter().any(|l| l.right) {
        return;
    }
    for line in laid.lines.iter_mut().filter(|l| l.right) {
        for p in &mut line.items {
            p.x += shift;
        }
    }
    laid.flush = edge;
}

/// One line at most `width` wide, cut at a grapheme with `…` after.
fn layout_cut(atoms: &[Atom], base: &Look, width: f32, ltr: bool, window: &Window) -> Laid {
    let mut laid = layout(atoms, base, f32::INFINITY, ltr, window);
    laid.lines.truncate(1);
    let Some(line) = laid.lines.first_mut() else {
        return laid;
    };
    let end = line
        .items
        .last()
        .map(|p| p.x + p.lead + p.width + p.tail)
        .unwrap_or(0.);
    if end <= width {
        laid.width = Some(width);
        laid.size.width = px(end.min(width).ceil());
        return laid;
    }
    // A right-to-left line keeps its start, on the right: cut it as its mirror image.
    let rtl_line = line.right;
    if rtl_line {
        mirror_items(&mut line.items, end);
    }
    let look_of = |i: usize| match &atoms[i] {
        Atom::Text { look, .. } => look.clone(),
        _ => base.clone(),
    };
    let mut keep = Vec::new();
    let mut cut_at = None;
    for p in line.items.drain(..) {
        let look = look_of(p.atom);
        let ell = f32::from(shape("…", &look, look.color, window).width);
        let room = width - ell;
        if p.x + p.lead + p.width + p.tail <= room {
            keep.push(p);
            continue;
        }
        if let Atom::Text { text, .. } = &atoms[p.atom] {
            let line = shape(text, &look, look.color, window);
            let base_x = f32::from(line.x_for_index(p.range.start));
            // Right-to-left text is laid out visually on the whole line: measure each cut alone.
            let rtl = has_rtl(text);
            let upto = |e: usize| {
                if rtl {
                    f32::from(shape(&text[p.range.start..e], &look, look.color, window).width)
                } else {
                    f32::from(line.x_for_index(e)) - base_x
                }
            };
            let mut end = p.range.start;
            for (off, g) in text[p.range.clone()].grapheme_indices(true) {
                let e = p.range.start + off + g.len();
                if p.x + p.lead + upto(e) > room {
                    break;
                }
                end = e;
            }
            let trimmed = text[p.range.start..end].trim_end().len() + p.range.start;
            if trimmed > p.range.start {
                let w = upto(trimmed);
                let x = p.x;
                let lead = p.lead;
                keep.push(Placed {
                    atom: p.atom,
                    range: p.range.start..trimmed,
                    x,
                    width: w,
                    lead,
                    tail: 0.,
                    mirror: false,
                });
                cut_at = Some((x + lead + w, p.atom));
                break;
            }
        }
        cut_at = Some((p.x, p.atom));
        break;
    }
    let cut = cut_at.unwrap_or((
        keep.last()
            .map(|p| p.x + p.lead + p.width + p.tail)
            .unwrap_or(0.),
        0,
    ));
    let cut = if rtl_line {
        mirror_items(&mut keep, width);
        let look = look_of(cut.1);
        let ell = f32::from(shape("…", &look, look.color, window).width);
        laid.flush = width;
        (width - cut.0 - ell, cut.1)
    } else {
        cut
    };
    line.items = keep;
    laid.ellipsis = Some(cut);
    laid.width = Some(width);
    laid.size.width = px(width);
    laid
}

/// Reflects a line's pieces within `width`, keeping them in left-to-right order.
fn mirror_items(items: &mut [Placed], width: f32) {
    for p in items.iter_mut() {
        p.x = width - (p.x + p.lead + p.width + p.tail);
        std::mem::swap(&mut p.lead, &mut p.tail);
    }
    items.reverse();
}

fn place(lines: &mut [Vec<Placed>], x: &mut f32, s: &Seg, atoms: &[Atom]) {
    let line = lines.last_mut().unwrap();
    if matches!(atoms[s.atom], Atom::Break) {
        return;
    }
    line.push(Placed {
        atom: s.atom,
        range: s.range.clone(),
        x: *x,
        width: s.width,
        lead: s.lead,
        tail: s.tail,
        mirror: false,
    });
    *x += s.lead + s.width + s.tail;
}

/// The text a screen reader reads: emoji as their characters or `:shortcode:`, a pill as its words.
fn spoken(atoms: &[Atom]) -> String {
    let mut out = String::new();
    for atom in atoms {
        match atom {
            Atom::Text { text, .. } => out.push_str(text),
            Atom::Image { alt, .. } => out.push_str(alt),
            Atom::Break => out.push('\n'),
            Atom::Pill(pill) => {
                for part in &pill.parts {
                    if let Part::Text { text, .. } = part {
                        out.push_str(text);
                    }
                }
            }
            Atom::Gap(_) | Atom::Ring(_) => {}
        }
    }
    out
}

pub struct Prepainted {
    hits: Vec<(Hitbox, Target)>,
    images: Vec<AnyElement>,
    /// Hover layers under the text: a tag's lift, a time's hint.
    under: Vec<AnyElement>,
}

impl Element for Inline {
    type RequestLayoutState = Rc<RefCell<Laid>>;
    type PrepaintState = Prepainted;

    fn id(&self) -> Option<ElementId> {
        self.id.clone()
    }

    fn source_location(&self) -> Option<&'static std::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        window: &mut Window,
        _: &mut App,
    ) -> (LayoutId, Self::RequestLayoutState) {
        let laid: Rc<RefCell<Laid>> = Rc::default();
        let (atoms, base, state, cut, nowrap, ltr) = (
            self.atoms.clone(),
            self.base.clone(),
            laid.clone(),
            self.ellipsis,
            self.nowrap,
            self.ltr,
        );
        let id =
            window.request_measured_layout(Style::default(), move |known, available, window, _| {
                let width = if nowrap {
                    f32::INFINITY
                } else {
                    known.width.map(f32::from).unwrap_or(match available.width {
                        gpui::AvailableSpace::Definite(w) => w.into(),
                        gpui::AvailableSpace::MinContent => 0.,
                        gpui::AvailableSpace::MaxContent => f32::INFINITY,
                    })
                };
                let mut s = state.borrow_mut();
                if s.width != Some(width) {
                    *s = if cut {
                        layout_cut(&atoms, &base, width, ltr, window)
                    } else {
                        layout(&atoms, &base, width, ltr, window)
                    };
                }
                s.size
            });
        (id, laid)
    }

    fn prepaint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        laid: &mut Self::RequestLayoutState,
        window: &mut Window,
        cx: &mut App,
    ) -> Prepainted {
        {
            let w = f32::from(bounds.size.width);
            let mut s = laid.borrow_mut();
            if !self.nowrap && s.width.is_none_or(|lw| {
                (lw - w).abs() > 0.5 && (self.ellipsis || f32::from(s.size.width) > w)
            }) {
                *s = if self.ellipsis {
                    layout_cut(&self.atoms, &self.base, w, self.ltr, window)
                } else {
                    layout(&self.atoms, &self.base, w, self.ltr, window)
                };
            }
            if !self.dir_auto && window.mirrored() {
                to_start(&mut s);
            }
            flush_to(&mut s, w);
        }
        if let Some(key) = &self.select {
            crate::select::place(key, &self.atoms, laid, bounds);
        }
        if window.is_a11y_active() {
            window.report_a11y_text(&spoken(&self.atoms), bounds);
        }
        let s = laid.borrow();
        if let Some(cell) = &self.clipped
            && cell.replace(s.ellipsis.is_some()) != s.ellipsis.is_some()
        {
            window.request_animation_frame();
        }
        let mut hits = Vec::new();
        let mut images = Vec::new();
        let mut under = Vec::new();
        // Hover and tooltip state outlive a frame, so their ids must too.
        let salt = {
            use std::hash::{Hash, Hasher};
            let mut h = std::collections::hash_map::DefaultHasher::new();
            format!("{:?}", self.id).hash(&mut h);
            for a in self.atoms.iter() {
                if let Atom::Text { text, .. } = a {
                    text.hash(&mut h);
                }
            }
            h.finish()
        };
        for line in &s.lines {
            // A box run (a chip) is one hit area per line.
            let mut k = 0;
            while k < line.items.len() {
                let p = &line.items[k];
                match &self.atoms[p.atom] {
                    Atom::Text { text, look, kind } => {
                        let mut end = k;
                        let mut range = p.range.clone();
                        while end + 1 < line.items.len() && line.items[end + 1].atom == p.atom {
                            end += 1;
                            range.end = line.items[end].range.end;
                        }
                        let last = &line.items[end];
                        let m = metrics(look, window);
                        let x0 = p.x;
                        let x1 = last.x + last.lead + last.width + last.tail;
                        let rect = Bounds::new(
                            bounds.origin + point(px(x0), px(line.baseline - m.ascent - 2.)),
                            size(px(x1 - x0), px(m.ascent + m.descent + 4.)),
                        );
                        if let Some(target) = kind.target(&text[..]) {
                            hits.push((window.insert_hitbox(rect, HitboxBehavior::Normal), target));
                        }
                        if matches!(kind, Kind::Spoiler { revealed: false, .. }) {
                            let mut el = gpui::StyledImage::object_fit(img(gpui::ImageSource::Render(sheen())), gpui::ObjectFit::Fill)
                                .size_full()
                                .rounded(px(3.))
                                .id(ElementId::Name(format!("inline-{salt}-sheen-{}", images.len()).into()))
                                .into_any_element();
                            el.prepaint_as_root(rect.origin, rect.size.map(Into::into), window, cx);
                            images.push(el);
                        }
                        let (lift, hint) = match kind {
                            Kind::Mention(_) => (true, None),
                            Kind::Time(hint) if !hint.is_empty() => (false, Some(hint.clone())),
                            Kind::LinkTip(_, tip) => (false, Some(tip.clone())),
                            Kind::Action(a) if a.as_ref() == "edited" => (false, Some(crate::t::msg_edited_tip().into())),
                            Kind::Action(a) if a.starts_with(EMOJI_MISSING) => (false, Some(crate::t::msg_emoji_missing_tip().into())),
                            _ => (false, None),
                        };
                        if lift || hint.is_some() {
                            let sz = size(rect.size.width, rect.size.height - px(2.));
                            let mut el = div()
                                .id(ElementId::Name(format!("inline-{salt}-{}", under.len()).into()))
                                .w(sz.width)
                                .h(sz.height)
                                .rounded(px(4.))
                                .when(lift, |d| d.hover(|s| s.bg(white_a(0.05))))
                                .when_some(hint, |d, hint| {
                                    d.tooltip(move |w, cx| crate::kit::tooltip(hint.clone())(w, cx))
                                })
                                .into_any_element();
                            el.prepaint_as_root(
                                rect.origin + point(px(0.), px(1.)),
                                sz.map(Into::into),
                                window,
                                cx,
                            );
                            under.push(el);
                        }
                        let _ = range;
                        k = end + 1;
                        continue;
                    }
                    Atom::Image {
                        src,
                        size: sz,
                        drop,
                        tip,
                        ..
                    } => {
                        let pic = img(src.clone());
                        let pic = if self.held { pic.face() } else { pic };
                        let pic = gpui::StyledImage::object_fit(pic.size(px(*sz)).aspect_ratio(1.), gpui::ObjectFit::Contain);
                        let mut el = match tip.clone() {
                            Some(tip) => div()
                                .id(ElementId::Name(format!("inline-{salt}-tip-{}", images.len()).into()))
                                .size(px(*sz))
                                .opacity(0.4)
                                .hover(|s| s.opacity(1.))
                                .tooltip(move |w, cx| crate::kit::tooltip(tip.text())(w, cx))
                                .child(pic)
                                .into_any_element(),
                            None => pic.into_any_element(),
                        };
                        let origin = bounds.origin + point(px(p.x), px(line.baseline - line.sink + drop - sz));
                        el.prepaint_as_root(
                            origin,
                            size(px(*sz), px(*sz)).map(Into::into),
                            window,
                            cx,
                        );
                        images.push(el);
                    }
                    Atom::Ring(pct) => {
                        let fill = pct.unwrap_or(5.).max(5.) / 100.;
                        let mut el = crate::attachments::spinner(
                            RING,
                            2.,
                            2.,
                            fill,
                            white_a(0.25),
                            white_a(0.85),
                        );
                        let origin = bounds.origin
                            + point(px(p.x + RING_GAP), px(line.baseline + RING_DROP - RING));
                        el.prepaint_as_root(
                            origin,
                            size(px(RING), px(RING)).map(Into::into),
                            window,
                            cx,
                        );
                        images.push(el);
                    }
                    Atom::Pill(pill) => {
                        let m = metrics(&pill.look, window);
                        let h = pill.look.line_height + 2.;
                        let lead = (pill.look.line_height - (m.ascent + m.descent)) / 2.;
                        let origin = bounds.origin + point(px(p.x), px(line.baseline - m.ascent - lead - 1.));
                        let sz = size(px(p.width), px(h));
                        hits.push((
                            window.insert_hitbox(Bounds::new(origin, sz), HitboxBehavior::Normal),
                            pill.target.clone(),
                        ));
                        let mut el = pill.element(
                            ElementId::Name(format!("inline-{salt}-pill-{}", images.len()).into()),
                            h,
                        );
                        el.prepaint_as_root(origin, sz.map(Into::into), window, cx);
                        images.push(el);
                    }
                    Atom::Break | Atom::Gap(_) => {}
                }
                k += 1;
            }
        }
        Prepainted { hits, images, under }
    }

    fn paint(
        &mut self,
        id: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        laid: &mut Self::RequestLayoutState,
        pre: &mut Prepainted,
        window: &mut Window,
        cx: &mut App,
    ) {
        let s = laid.borrow();
        for el in &mut pre.under {
            el.paint(window, cx);
        }
        let action_hot = pre.hits.iter().any(|(h, t)| matches!(t, Target::Action(_)) && h.is_hovered(window));
        if let Some(key) = &self.select
            && let Some(picked) = crate::select::range_in(key, || plain_len(&self.atoms))
        {
            for r in rects(&self.atoms, &s, picked, window) {
                window.paint_quad(fill(Bounds::new(bounds.origin + r.origin, r.size), ca(89, 252, 179, 0.28)));
            }
        }
        for line in &s.lines {
            // Boxes first, so text paints over them.
            let mut k = 0;
            while k < line.items.len() {
                let p = &line.items[k];
                if let Atom::Text { look, kind, .. } = &self.atoms[p.atom] {
                    let mut end = k;
                    while end + 1 < line.items.len() && line.items[end + 1].atom == p.atom {
                        end += 1;
                    }
                    let last = &line.items[end];
                    let x0 = p.x;
                    let x1 = last.x + last.lead + last.width + last.tail;
                    let m = metrics(look, window);
                    let boxed = |pad_y: f32| {
                        Bounds::new(
                            bounds.origin + point(px(x0), px(line.baseline - m.ascent - pad_y)),
                            size(px(x1 - x0), px(m.ascent + m.descent + pad_y * 2.)),
                        )
                    };
                    if let Some(bg) = look.bg {
                        window.paint_quad(fill(boxed(1.), bg).corner_radii(Corners::all(px(3.))));
                    }
                    match kind {
                        Kind::Code => {
                            let b = boxed(3.);
                            window.paint_quad(
                                fill(b, ca(89, 252, 179, 0.05)).corner_radii(Corners::all(px(3.))),
                            );
                            window.paint_quad(
                                outline(b, ca(89, 252, 179, 0.45), gpui::BorderStyle::Solid)
                                    .corner_radii(Corners::all(px(3.)))
                                    .border_widths(Edges::all(px(1.))),
                            );
                        }
                        Kind::Spoiler {
                            revealed: false, ..
                        } => {
                            window.paint_quad(
                                fill(boxed(2.), ca(0, 0, 0, 0.35))
                                    .corner_radii(Corners::all(px(3.))),
                            );
                        }
                        Kind::Mention(_) | Kind::Tag(None) => {
                            window.paint_quad(
                                fill(boxed(1.), white_a(0.075)).corner_radii(Corners::all(px(4.))),
                            );
                        }
                        Kind::Tag(Some(color)) => {
                            let tint: Hsla = gpui::rgb(*color).into();
                            window.paint_quad(
                                fill(boxed(1.), tint.opacity(0.16)).corner_radii(Corners::all(px(4.))),
                            );
                        }
                        Kind::Time(_) => {
                            window.paint_quad(
                                fill(boxed(0.), white_a(0.09)).corner_radii(Corners::all(px(4.))),
                            );
                        }
                        _ => {}
                    }
                    k = end + 1;
                    continue;
                }
                k += 1;
            }
            for p in &line.items {
                let Atom::Text { text, look, kind } = &self.atoms[p.atom] else {
                    continue;
                };
                let m = metrics(look, window);
                let lh = look.line_height;
                let top = line.baseline - m.ascent - (lh - (m.ascent + m.descent)) / 2. - look.rise;
                let origin = bounds.origin + point(px(p.x + p.lead), px(top));
                let mirrored: String;
                let piece = if p.mirror {
                    mirrored = text[p.range.clone()].chars().map(|c| unicode_bidi_mirroring::get_mirrored(c).unwrap_or(c)).collect();
                    mirrored.as_str()
                } else {
                    &text[p.range.clone()]
                };
                match kind {
                    Kind::Spoiler {
                        revealed: false, ..
                    } => {
                        let line = shape(piece, look, look.color, window);
                        spoiler_cover(&line, piece, look, origin, m, 1., window);
                    }
                    Kind::Spoiler { id, revealed: true } if let Some(t) = self.select.as_ref().and_then(|k| revealing(k.msg, *id)) => {
                        // `spoilerReveal`: the cover thins out as the text comes back from the accent.
                        window.request_animation_frame();
                        let e = crate::kit::bezier(0., 0., 0.58, 1.)(t);
                        let accent = mix(c(ACCENT), c(0x04110d), 0.3);
                        let color = mix(accent, look.color, e).opacity(e);
                        let line = shape(piece, look, color, window);
                        spoiler_cover(&line, piece, look, origin, m, 1. - e, window);
                        let _ = line.paint(origin, px(lh), TextAlign::Left, None, window, cx);
                    }
                    Kind::Action(_) if action_hot => {
                        let line = shape(piece, look, white_a(0.7), window);
                        let _ = line.paint(origin, px(lh), TextAlign::Left, None, window, cx);
                    }
                    _ => {
                        let line = shape(piece, look, look.color, window);
                        let _ = line.paint(origin, px(lh), TextAlign::Left, None, window, cx);
                    }
                }
            }
        }
        if let (Some((x, atom)), Some(line)) = (s.ellipsis, s.lines.first()) {
            let look = match &self.atoms[atom] {
                Atom::Text { look, .. } => look.clone(),
                _ => self.base.clone(),
            };
            let m = metrics(&look, window);
            let top = line.baseline - m.ascent - (look.line_height - (m.ascent + m.descent)) / 2.;
            let _ = shape("…", &look, look.color, window).paint(
                bounds.origin + point(px(x), px(top)),
                px(look.line_height),
                TextAlign::Left,
                None,
                window,
                cx,
            );
        }
        for el in &mut pre.images {
            el.paint(window, cx);
        }

        if pre.hits.is_empty() {
            return;
        }
        for (hitbox, _) in &pre.hits {
            window.set_cursor_style(CursorStyle::PointingHand, hitbox);
        }
        let Some(on_click) = self.on_click.clone() else {
            return;
        };
        let pressed: Rc<Cell<Option<usize>>> = match id {
            Some(id) => {
                window.with_element_state(id, |state: Option<Rc<Cell<Option<usize>>>>, _| {
                    let state = state.unwrap_or_default();
                    (state.clone(), state)
                })
            }
            None => Rc::default(),
        };
        let hits: Rc<Vec<(Hitbox, Target)>> = Rc::new(std::mem::take(&mut pre.hits));
        {
            let (hits, pressed) = (hits.clone(), pressed.clone());
            window.on_mouse_event(move |e: &MouseDownEvent, phase, window, _| {
                if phase.bubble() && e.button == MouseButton::Left {
                    pressed.set(hits.iter().position(|(h, _)| h.is_hovered(window)));
                }
            });
        }
        window.on_mouse_event(move |e: &MouseUpEvent, phase, window, cx| {
            if !phase.bubble() || e.button != MouseButton::Left {
                return;
            }
            let Some(i) = pressed.take() else { return };
            if hits[i].0.is_hovered(window) {
                on_click(hits[i].1.clone(), window, cx);
            }
        });
    }
}

thread_local!(static REVEALS: RefCell<std::collections::HashMap<(u64, u32), std::time::Instant>> = RefCell::default());

/// A spoiler just clicked open in message `msg`: it eases out of its cover over 0.3 s.
pub fn reveal(msg: &str, id: u32) {
    REVEALS.with_borrow_mut(|r| {
        r.retain(|_, at| at.elapsed().as_secs_f32() < 1.);
        r.insert((crate::select::msg_hash(msg), id), std::time::Instant::now());
    });
}

/// How far a spoiler's reveal has run, while it runs.
fn revealing(msg: u64, id: u32) -> Option<f32> {
    let t = REVEALS.with_borrow(|r| r.get(&(msg, id)).map(|at| at.elapsed().as_secs_f32() / 0.3))?;
    (t < 1.).then_some(t)
}

fn mix(a: Hsla, b: Hsla, t: f32) -> Hsla {
    let (a, b) = (a.to_rgb(), b.to_rgb());
    gpui::Rgba {
        r: a.r + (b.r - a.r) * t,
        g: a.g + (b.g - a.g) * t,
        b: a.b + (b.b - a.b) * t,
        a: 1.,
    }
    .into()
}

/// Plain text with its emoji drawn as twemoji at `emoji` px, on one line cut with `…`.
pub fn emoji_line(text: &str, look: Look, emoji: f32, drop: f32) -> Inline {
    let text = &*crate::emoji::plain_text_symbols(text);
    let mut atoms = Vec::new();
    let mut run = String::new();
    for g in unicode_segmentation::UnicodeSegmentation::graphemes(text, true) {
        match crate::emoji::twemoji(g) {
            Some(src) => {
                if !run.is_empty() {
                    atoms.push(Atom::Text {
                        text: std::mem::take(&mut run).into(),
                        look: look.clone(),
                        kind: Kind::Plain,
                    });
                }
                atoms.push(Atom::Image {
                    src: src.into(),
                    size: emoji,
                    drop,
                    tip: None,
                    alt: g.to_string().into(),
                });
            }
            None => run.push_str(g),
        }
    }
    if !run.is_empty() {
        atoms.push(Atom::Text {
            text: run.into(),
            look: look.clone(),
            kind: Kind::Plain,
        });
    }
    inline(atoms, look).single_line()
}

/// A hidden spoiler's text as `filter: blur(4px)` leaves it: no glyph drawn, each letter a
/// soft accent smudge as tall as its kind of letter, so words keep their shape and lose their
/// letters, under the `text-shadow` glow.
fn spoiler_cover(line: &ShapedLine, piece: &str, look: &Look, origin: gpui::Point<Pixels>, m: Metrics, alpha: f32, window: &mut Window) {
    if alpha <= 0. {
        return;
    }
    let fs = look.size;
    let baseline = f32::from(origin.y) + (look.line_height - (m.ascent + m.descent)) / 2. + m.ascent;
    let blur = px(fs * 0.24);
    let shadow = |color: Hsla| gpui::BoxShadow { color, offset: point(px(0.), px(0.)), blur_radius: blur, spread_radius: px(0.), inset: false };
    let (mut lo, mut hi) = (f32::MAX, f32::MIN);
    for (i, ch) in piece.char_indices() {
        if ch.is_whitespace() {
            continue;
        }
        let x0 = f32::from(origin.x + line.x_for_index(i));
        let x1 = f32::from(origin.x + line.x_for_index(i + ch.len_utf8()));
        if x1 <= x0 {
            continue;
        }
        (lo, hi) = (lo.min(x0), hi.max(x1));
        let tall = !ch.is_ascii_lowercase() || "bdfhiklt".contains(ch);
        let low = "gjpqy".contains(ch);
        let top = baseline - fs * if tall { 0.7 } else { 0.5 };
        let bottom = baseline + if low { fs * 0.2 } else { 0. };
        let pad = (x1 - x0) * 0.1;
        let b = Bounds::new(point(px(x0 + pad), px(top)), size(px(x1 - x0 - 2. * pad), px(bottom - top)));
        window.paint_drop_shadows(b, Corners::all(px(fs * 0.1)), &[shadow(mix(c(ACCENT), c(0x04110d), 0.2).opacity(0.7 * alpha))]);
    }
    if lo < hi {
        let b = Bounds::new(point(px(lo), px(baseline - fs * 0.6)), size(px(hi - lo), px(fs * 0.7)));
        window.paint_drop_shadows(b, Corners::all(px(fs * 0.3)), &[gpui::BoxShadow { blur_radius: px(14.), ..shadow(c(ACCENT).opacity(0.22 * alpha)) }]);
    }
}

/// The light that sweeps across a hidden spoiler every four seconds, as frames: drawn
/// stretched over each box and stepped like any animated picture, so the shimmer swaps a tile
/// rather than redrawing the chat.
fn sheen() -> std::sync::Arc<gpui::RenderImage> {
    thread_local! {
        static SHEEN: std::sync::Arc<gpui::RenderImage> = {
            const W: u32 = 64;
            const FRAMES: u32 = 40;
            let frames: Vec<image::Frame> = (0..FRAMES)
                .map(|f| {
                    let centre = -0.4 + 1.8 * f as f32 / FRAMES as f32;
                    let mut buf = image::RgbaImage::new(W, 2);
                    for (x, _, px) in buf.enumerate_pixels_mut() {
                        let d = (x as f32 / (W - 1) as f32 - centre) / 0.2;
                        let a = (0.5 * (-d * d).exp() * 255.) as u8;
                        // BGRA, as the atlas takes it: the accent, 0x59fcb3.
                        *px = image::Rgba([0xb3, 0xfc, 0x59, a]);
                    }
                    image::Frame::from_parts(buf, 0, 0, image::Delay::from_numer_denom_ms(4000 / FRAMES, 1))
                })
                .collect();
            std::sync::Arc::new(gpui::RenderImage::new(frames))
        };
    }
    SHEEN.with(|s| s.clone())
}

/// Bidi embedding levels for every byte of a block, each paragraph resolved at level 0 as a
/// page without `dir` lays it out, or, while the UI reads right to left, at the level its first
/// letter sets (`dir="auto"`). Boxes (emoji, pills) stand in as U+FFFC.
struct Bidi {
    starts: Vec<usize>,
    levels: Vec<u8>,
    /// Byte ranges of the paragraphs that read right to left.
    rtl: Vec<Range<usize>>,
}

impl Bidi {
    fn para_level(&self, at: usize) -> u8 {
        self.rtl.iter().any(|r| r.contains(&at)) as u8
    }
}

/// `None` when nothing in the paragraph runs right to left, which leaves its lines as laid.
fn bidi(atoms: &[Atom], ltr: bool) -> Option<Bidi> {
    let mut text = String::new();
    let mut starts = Vec::with_capacity(atoms.len());
    for a in atoms {
        starts.push(text.len());
        match a {
            Atom::Text { text: t, .. } => text.push_str(t),
            Atom::Break => text.push('\n'),
            Atom::Gap(_) => {}
            _ => text.push('\u{FFFC}'),
        }
    }
    if !has_rtl(&text) {
        return None;
    }
    let base = ltr.then(unicode_bidi::Level::ltr);
    let info = unicode_bidi::BidiInfo::new(&text, base);
    let rtl = info.paragraphs.iter().filter(|p| p.level.is_rtl()).map(|p| p.range.clone()).collect();
    Some(Bidi { starts, levels: info.levels.iter().map(|l| l.number()).collect(), rtl })
}

/// A line's pieces cut where their level changes, and spaces apart from words, so each cut
/// shapes in one direction. Returns (index into `line`, byte range, level) in logical order,
/// or `None` when the whole line sits at level 0.
fn bidi_cuts(line: &[Placed], atoms: &[Atom], b: &Bidi) -> Option<Vec<(usize, Range<usize>, u8)>> {
    let level = |atom: usize, at: usize| b.levels.get(b.starts[atom] + at).copied().unwrap_or(0);
    let mut out = Vec::with_capacity(line.len());
    let mut any = false;
    for (i, p) in line.iter().enumerate() {
        let Atom::Text { text, .. } = &atoms[p.atom] else {
            let l = level(p.atom, 0);
            any |= l > 0;
            out.push((i, p.range.clone(), l));
            continue;
        };
        let piece = &text[p.range.clone()];
        if piece.char_indices().all(|(k, _)| level(p.atom, p.range.start + k) == 0) {
            out.push((i, p.range.clone(), 0));
            continue;
        }
        any = true;
        let mut last: Option<(u8, bool)> = None;
        for (k, ch) in piece.char_indices() {
            let at = p.range.start + k;
            let key = (level(p.atom, at), ch.is_whitespace());
            match out.last_mut() {
                Some((j, r, _)) if *j == i && last == Some(key) => r.end = at + ch.len_utf8(),
                _ => out.push((i, at..at + ch.len_utf8(), key.0)),
            }
            last = Some(key);
        }
    }
    if !any {
        return None;
    }
    // L1: spaces ending the line take the paragraph's level.
    let para = line.first().map_or(0, |p| b.para_level(b.starts[p.atom] + p.range.start));
    for (i, r, l) in out.iter_mut().rev() {
        match &atoms[line[*i].atom] {
            Atom::Text { text, .. } if text[r.clone()].chars().all(char::is_whitespace) => *l = para,
            _ => break,
        }
    }
    Some(out)
}

/// L2: from the highest level down to the lowest odd one, each run at that level or above
/// reverses.
fn visual_order(levels: &[u8]) -> Vec<usize> {
    let mut order: Vec<usize> = (0..levels.len()).collect();
    let (Some(&max), Some(&min)) = (levels.iter().max(), levels.iter().min()) else {
        return order;
    };
    let lowest_odd = if min % 2 == 1 { min } else { min + 1 };
    for lvl in (lowest_odd..=max).rev() {
        let mut i = 0;
        while i < order.len() {
            if levels[order[i]] < lvl {
                i += 1;
                continue;
            }
            let mut j = i;
            while j < order.len() && levels[order[j]] >= lvl {
                j += 1;
            }
            order[i..j].reverse();
            i = j;
        }
    }
    order
}

/// A line in visual order: pieces cut by level, measured, reversed run by run, then laid
/// from the line's start.
fn reorder(line: &mut Vec<Placed>, atoms: &[Atom], b: &Bidi, window: &Window) {
    let Some(cuts) = bidi_cuts(line, atoms, b) else {
        return;
    };
    let start = line.first().map_or(0., |p| p.x);
    let levels: Vec<u8> = cuts.iter().map(|c| c.2).collect();
    let mut units: Vec<Placed> = Vec::with_capacity(cuts.len());
    for (k, (i, range, level)) in cuts.iter().enumerate() {
        let p = &line[*i];
        let first = k == 0 || cuts[k - 1].0 != *i;
        let last = k + 1 == cuts.len() || cuts[k + 1].0 != *i;
        let (width, mirror) = match &atoms[p.atom] {
            Atom::Text { text, look, .. } => {
                let piece = &text[range.clone()];
                let mirror = level % 2 == 1 && !has_rtl(piece) && piece.chars().any(unicode_bidi_mirroring::is_mirroring);
                let width = if first && last { p.width } else { f32::from(shape(piece, look, look.color, window).width) };
                (width, mirror)
            }
            _ => (p.width, false),
        };
        units.push(Placed {
            atom: p.atom,
            range: range.clone(),
            x: 0.,
            width,
            lead: if first { p.lead } else { 0. },
            tail: if last { p.tail } else { 0. },
            mirror,
        });
    }
    let mut slots: Vec<Option<Placed>> = units.into_iter().map(Some).collect();
    let mut x = start;
    *line = visual_order(&levels)
        .into_iter()
        .filter_map(|k| slots[k].take())
        .map(|mut p| {
            p.x = x;
            x += p.lead + p.width + p.tail;
            p
        })
        .collect();
}

/// How much of the paragraph's plain text an atom stands for.
fn atom_len(a: &Atom) -> usize {
    match a {
        Atom::Text { text, .. } => text.len(),
        Atom::Image { alt, .. } => alt.len(),
        Atom::Pill(p) => p.parts.iter().map(|p| if let Part::Text { text, .. } = p { text.len() } else { 0 }).sum(),
        Atom::Break => 1,
        Atom::Gap(_) | Atom::Ring(_) => 0,
    }
}

fn plain_len(atoms: &[Atom]) -> usize {
    atoms.iter().map(atom_len).sum()
}

fn starts(atoms: &[Atom]) -> Vec<usize> {
    let mut at = 0;
    atoms
        .iter()
        .map(|a| {
            let s = at;
            at += atom_len(a);
            s
        })
        .collect()
}

/// A paragraph as plain text: an emoji as itself, a pill as its words, a break as a newline.
pub(crate) fn plain(atoms: &[Atom]) -> String {
    let mut out = String::with_capacity(plain_len(atoms));
    for a in atoms {
        match a {
            Atom::Text { text, .. } => out.push_str(text),
            Atom::Image { alt, .. } => out.push_str(alt),
            Atom::Pill(p) => {
                for part in &p.parts {
                    if let Part::Text { text, .. } = part {
                        out.push_str(text);
                    }
                }
            }
            Atom::Break => out.push('\n'),
            Atom::Gap(_) | Atom::Ring(_) => {}
        }
    }
    out
}

/// The plain-text offset nearest `local`, a point in the paragraph's own space.
pub(crate) fn hit(atoms: &[Atom], laid: &Laid, local: gpui::Point<Pixels>, window: &Window) -> usize {
    let (Some(first), Some(last)) = (laid.lines.first(), laid.lines.last()) else { return 0 };
    let (x, y) = (f32::from(local.x), f32::from(local.y));
    if y < first.top {
        return 0;
    }
    if y >= last.top + last.height {
        return plain_len(atoms);
    }
    let starts = starts(atoms);
    let line = laid.lines.iter().find(|l| y < l.top + l.height).unwrap_or(last);
    for (n, p) in line.items.iter().enumerate() {
        let right = p.x + p.lead + p.width + p.tail;
        if x >= right && n + 1 < line.items.len() {
            continue;
        }
        return match &atoms[p.atom] {
            Atom::Text { text, look, .. } => {
                let shaped = shape(&text[p.range.clone()], look, look.color, window);
                starts[p.atom] + p.range.start + shaped.closest_index_for_x(px(x - p.x - p.lead))
            }
            a => starts[p.atom] + if x < (p.x + right) / 2. { 0 } else { atom_len(a) },
        };
    }
    // An empty line (a break alone): where the next one starts.
    let next = laid.lines.iter().skip_while(|l| !std::ptr::eq(*l, line)).skip(1).find_map(|l| l.items.first());
    next.map_or(plain_len(atoms), |p| starts[p.atom] + p.range.start)
}

/// Where the plain-text span `picked` is drawn, in the paragraph's own space: one box per line,
/// so neighbouring pieces' translucent fills don't overlap into seams.
fn rects(atoms: &[Atom], laid: &Laid, picked: Range<usize>, window: &Window) -> Vec<Bounds<Pixels>> {
    let starts = starts(atoms);
    let mut out = Vec::new();
    for line in &laid.lines {
        let mut span: Option<(f32, f32)> = None;
        for p in &line.items {
            let (a0, a1) = match &atoms[p.atom] {
                Atom::Text { .. } => (starts[p.atom] + p.range.start, starts[p.atom] + p.range.end),
                a => (starts[p.atom], starts[p.atom] + atom_len(a)),
            };
            let (lo, hi) = (picked.start.max(a0), picked.end.min(a1));
            if lo >= hi {
                continue;
            }
            let (x0, x1) = match &atoms[p.atom] {
                Atom::Text { text, look, .. } => {
                    let shaped = shape(&text[p.range.clone()], look, look.color, window);
                    let base = p.x + p.lead;
                    (base + f32::from(shaped.x_for_index(lo - a0)), base + f32::from(shaped.x_for_index(hi - a0)))
                }
                _ => (p.x, p.x + p.lead + p.width + p.tail),
            };
            if x1 > x0 {
                span = Some(span.map_or((x0, x1), |(a, b)| (a.min(x0), b.max(x1))));
            }
        }
        if let Some((x0, x1)) = span {
            out.push(Bounds::new(point(px(x0), px(line.top)), size(px(x1 - x0), px(line.height))));
        }
    }
    out
}

#[cfg(test)]
mod rtl_tests {
    use super::*;

    /// The pieces of one line of `text`, cut after each space as the line breaker does, in
    /// the order they are drawn left to right.
    fn order(text: &str) -> Vec<String> {
        let atoms = vec![Atom::Text { text: text.to_string().into(), look: Look::new("Rubik", 15., 21., gpui::white()), kind: Kind::Plain }]; // i18n-exempt: font name
        let mut cuts = vec![0];
        cuts.extend(text.match_indices(' ').map(|(i, _)| i + 1));
        cuts.push(text.len());
        cuts.dedup();
        let line: Vec<Placed> =
            cuts.windows(2).map(|w| Placed { atom: 0, range: w[0]..w[1], x: 0., width: 10., lead: 0., tail: 0., mirror: false }).collect();
        let Some(b) = bidi(&atoms, true) else {
            return line.iter().map(|p| text[p.range.clone()].to_string()).collect();
        };
        let Some(cuts) = bidi_cuts(&line, &atoms, &b) else {
            return line.iter().map(|p| text[p.range.clone()].to_string()).collect();
        };
        let levels: Vec<u8> = cuts.iter().map(|c| c.2).collect();
        visual_order(&levels).into_iter().map(|k| text[cuts[k].1.clone()].to_string()).collect()
    }

    #[test]
    fn a_right_to_left_line_reads_right_to_left() {
        assert_eq!(order("שלום עולם יפה").concat(), "יפה עולם שלום");
    }

    #[test]
    fn mixed_lines_reverse_only_the_right_to_left_run() {
        assert_eq!(order("hi שלום עולם there").concat(), "hi עולם שלום there"); // i18n-exempt: fixture
        assert_eq!(order("just english words").concat(), "just english words"); // i18n-exempt: fixture
    }

    #[test]
    fn numbers_between_right_to_left_words_stay_in_the_run() {
        assert_eq!(order("שלום 42 עולם").concat(), "עולם 42 שלום");
    }

    #[test]
    fn punctuation_before_english_stays_after_the_hebrew() {
        // UAX #9 N1/N2: "!" between Hebrew and English takes the paragraph's direction.
        assert_eq!(order("שלום עולם! Hello world.").concat(), "עולם שלום! Hello world."); // i18n-exempt: fixture
        assert_eq!(order("Meet at בית הספר at 5pm.").concat(), "Meet at הספר בית at 5pm."); // i18n-exempt: fixture
    }

    #[test]
    fn a_wrapped_lines_end_space_takes_the_paragraph_level() {
        let atoms = vec![Atom::Text { text: "שלום עולם".into(), look: Look::new("Rubik", 15., 21., gpui::white()), kind: Kind::Plain }]; // i18n-exempt: font name
        let b = bidi(&atoms, true).unwrap();
        let line = vec![Placed { atom: 0, range: 0..9, x: 0., width: 10., lead: 0., tail: 0., mirror: false }];
        let cuts = bidi_cuts(&line, &atoms, &b).unwrap();
        assert_eq!(cuts.iter().map(|c| c.2).collect::<Vec<_>>(), vec![1, 0]);
    }

    #[test]
    fn levels_reverse_from_the_highest_down() {
        assert_eq!(visual_order(&[0, 1, 2, 2, 1, 0]), vec![0, 4, 2, 3, 1, 5]);
        assert_eq!(visual_order(&[1, 1, 1]), vec![2, 1, 0]);
    }

    #[cfg(target_os = "linux")]
    fn platform_text() -> std::sync::Arc<dyn gpui::PlatformTextSystem> {
        std::sync::Arc::new(gpui_wgpu::CosmicTextSystem::new("Rubik")) // i18n-exempt: font name
    }

    #[cfg(target_os = "macos")]
    fn platform_text() -> std::sync::Arc<dyn gpui::PlatformTextSystem> {
        std::sync::Arc::new(gpui_apple::AppleTextSystem::new())
    }

    #[cfg(target_os = "windows")]
    fn platform_text() -> std::sync::Arc<dyn gpui::PlatformTextSystem> {
        gpui_windows::text_system().expect("DirectWrite")
    }

    /// Each OS's shaper sets a Hebrew word inside an English line right to left, between the
    /// words around it, with the glyphs of every run in left-to-right order.
    #[test]
    fn the_platform_shaper_orders_a_right_to_left_run() {
        let ts = platform_text();
        ts.add_fonts(vec![std::borrow::Cow::Borrowed(include_bytes!("../assets/fonts/Rubik-Regular.ttf"))]).unwrap();
        let id = ts.font_id(&gpui::font("Rubik")).unwrap(); // i18n-exempt: font name
        let text = "ab \u{5d0}\u{5d1}\u{5d2} cd";
        let line = ts.layout_line(text, px(16.), &[gpui::FontRun { len: text.len(), font_id: id }]);
        let glyphs: Vec<_> = line.runs.iter().flat_map(|r| r.glyphs.iter()).collect();
        let x = |i: usize| glyphs.iter().find(|g| g.index == i).map(|g| f32::from(g.position.x)).unwrap_or_else(|| panic!("no glyph at {i}: {glyphs:?}"));
        let order = [x(0), x(1), x(7), x(5), x(3), x(10), x(11)];
        assert!(order.windows(2).all(|w| w[0] < w[1]), "a b \u{5d2} \u{5d1} \u{5d0} c d at {order:?}");
        for run in &line.runs {
            assert!(run.glyphs.windows(2).all(|w| w[0].position.x <= w[1].position.x), "a run out of order: {:?}", run.glyphs);
        }
        assert!(f32::from(line.width) >= x(11), "width {:?} short of the last glyph", line.width);
    }
}
