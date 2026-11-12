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

use gpui::{
    AnyElement, App, Bounds, Corners, CursorStyle, Edges, Element, ElementId, Font, FontStyle,
    FontWeight, GlobalElementId, Hitbox, HitboxBehavior, Hsla, InspectorElementId, IntoElement,
    LayoutId, MouseButton, MouseDownEvent, MouseUpEvent, Pixels, ShapedLine, SharedString, Size,
    StrikethroughStyle, Style, Styled, TextAlign, TextRun, Window, fill, img, outline, point, px,
    size, div, InteractiveElement, StatefulInteractiveElement, ParentElement, prelude::FluentBuilder,
};
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
            underline: None,
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
                    format!("Message dissolves in {}", crate::timeline::countdown(left as u64))
                } else {
                    "Dissolving...".into()
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
    },
    /// Fixed space that never breaks from what follows (an inline box's margin).
    Gap(f32),
    /// A linked picture's fetch (`.inline-image-loading`): 6px off the text, a 16px ring.
    Ring(Option<f32>),
    /// A forced line break.
    Break,
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
}

struct Line {
    top: f32,
    height: f32,
    baseline: f32,
    items: Vec<Placed>,
}

#[derive(Default)]
pub struct Laid {
    width: Option<f32>,
    lines: Vec<Line>,
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
        descent: ts.descent(id, s).into(),
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
                    let full = f32::from(line.x_for_index(end) - line.x_for_index(start));
                    let body = f32::from(
                        line.x_for_index(start + trimmed.len()) - line.x_for_index(start),
                    );
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
    let mut cut = None;
    for (off, g) in text[seg.range.clone()].grapheme_indices(true) {
        let end = seg.range.start + off + g.len();
        if end == seg.range.end {
            break;
        }
        if f32::from(line.x_for_index(end)) - base + seg.lead <= room {
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
    let line = shape(text, look, look.color, window);
    f32::from(line.x_for_index(range.end) - line.x_for_index(range.start))
}

fn layout(atoms: &[Atom], base: &Look, width: f32, window: &Window) -> Laid {
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

    // Vertical: every line holds the block's strut, then each box by its own line-height.
    let strut = metrics(base, window);
    let half = |m: &Metrics, lh: f32| (lh - (m.ascent + m.descent)) / 2.;
    let strut_above = strut.ascent + half(&strut, base.line_height);
    let strut_below = strut.descent + half(&strut, base.line_height);
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
                    above = above.max(size - drop);
                    below = below.max(*drop);
                }
                Atom::Ring(_) => {
                    above = above.max(RING - RING_DROP);
                    below = below.max(RING_DROP);
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
            items,
        });
        top += height;
    }
    Laid {
        width: Some(width),
        lines: out,
        ellipsis: None,
        size: size(px(widest.min(width).ceil()), px(top)),
    }
}

/// One line at most `width` wide, cut at a grapheme with `…` after.
fn layout_cut(atoms: &[Atom], base: &Look, width: f32, window: &Window) -> Laid {
    let mut laid = layout(atoms, base, f32::INFINITY, window);
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
            let mut end = p.range.start;
            for (off, g) in text[p.range.clone()].grapheme_indices(true) {
                let e = p.range.start + off + g.len();
                if p.x + p.lead + f32::from(line.x_for_index(e)) - base_x > room {
                    break;
                }
                end = e;
            }
            let trimmed = text[p.range.start..end].trim_end().len() + p.range.start;
            if trimmed > p.range.start {
                let w = f32::from(line.x_for_index(trimmed)) - base_x;
                let x = p.x;
                let lead = p.lead;
                keep.push(Placed {
                    atom: p.atom,
                    range: p.range.start..trimmed,
                    x,
                    width: w,
                    lead,
                    tail: 0.,
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
    line.items = keep;
    laid.ellipsis = Some(cut);
    laid.width = Some(width);
    laid.size.width = px(width);
    laid
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
    });
    *x += s.lead + s.width + s.tail;
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
        let (atoms, base, state, cut) = (
            self.atoms.clone(),
            self.base.clone(),
            laid.clone(),
            self.ellipsis,
        );
        let id =
            window.request_measured_layout(Style::default(), move |known, available, window, _| {
                let width = known.width.map(f32::from).unwrap_or(match available.width {
                    gpui::AvailableSpace::Definite(w) => w.into(),
                    gpui::AvailableSpace::MinContent => 0.,
                    gpui::AvailableSpace::MaxContent => f32::INFINITY,
                });
                let mut s = state.borrow_mut();
                if s.width != Some(width) {
                    *s = if cut {
                        layout_cut(&atoms, &base, width, window)
                    } else {
                        layout(&atoms, &base, width, window)
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
            if s.width.is_none_or(|lw| {
                (lw - w).abs() > 0.5 && (self.ellipsis || f32::from(s.size.width) > w)
            }) {
                *s = if self.ellipsis {
                    layout_cut(&self.atoms, &self.base, w, window)
                } else {
                    layout(&self.atoms, &self.base, w, window)
                };
            }
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
                        let (lift, hint) = match kind {
                            Kind::Mention(_) => (true, None),
                            Kind::Time(hint) if !hint.is_empty() => (false, Some(hint.clone())),
                            Kind::LinkTip(_, tip) => (false, Some(tip.clone())),
                            Kind::Action(a) if a.as_ref() == "edited" => (false, Some("Click to view edit history".into())),
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
                    } => {
                        let pic = gpui::StyledImage::object_fit(
                            img(src.clone()).size(px(*sz)).aspect_ratio(1.),
                            gpui::ObjectFit::Contain,
                        );
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
                        let origin = bounds.origin + point(px(p.x), px(line.baseline + drop - sz));
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
        let mut shimmering = false;
        for el in &mut pre.under {
            el.paint(window, cx);
        }
        let action_hot = pre.hits.iter().any(|(h, t)| matches!(t, Target::Action(_)) && h.is_hovered(window));
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
                            shimmering = true;
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
                let top = line.baseline - m.ascent - (lh - (m.ascent + m.descent)) / 2.;
                let origin = bounds.origin + point(px(p.x + p.lead), px(top));
                let piece = &text[p.range.clone()];
                match kind {
                    Kind::Spoiler {
                        revealed: false, ..
                    } => {
                        // No blur in GPUI: offset copies at low alpha read as one, in the
                        // shimmering accent the CSS gradient sweeps across.
                        let phase = (self.clock / 4.).fract();
                        let mid = (p.x + p.width / 2.) / s.size.width.max(px(1.)).to_f64() as f32;
                        let t = ((mid - phase * 2.).rem_euclid(1.) * 2. - 1.).abs();
                        let color = mix(c(ACCENT), c(0x04110d), 1. - t);
                        for (dx, dy) in [
                            (-1.5f32, -1.),
                            (1.5, -1.),
                            (-1.5, 1.),
                            (1.5, 1.),
                            (0., 0.),
                            (-3., 0.),
                            (3., 0.),
                            (0., -2.),
                            (0., 2.),
                        ] {
                            let line = shape(piece, look, color.opacity(0.22), window);
                            let _ = line.paint(
                                origin + point(px(dx), px(dy)),
                                px(lh),
                                TextAlign::Left,
                                None,
                                window,
                                cx,
                            );
                        }
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
        if shimmering {
            window.request_animation_frame();
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
