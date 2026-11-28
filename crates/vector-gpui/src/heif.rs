//! HEIF and HEIC pictures. The container is read here; FFmpeg decodes the HEVC tiles a row of
//! tiles at a time, and each row is cropped, converted and scaled as it lands, so a 48 MP photo
//! never sits in memory whole. Sequences play through the animation path (`media`).

#![cfg_attr(not(feature = "video"), allow(dead_code))]

use std::borrow::Cow;

/// Brands that promise HEVC pictures or sequences.
const HEVC: [&[u8; 4]; 6] = [b"heic", b"heix", b"heim", b"heis", b"hevc", b"hevx"];
/// Brands any HEIF file may carry, AVIF's included.
const GENERIC: [&[u8; 4]; 2] = [b"mif1", b"msf1"];
const AV1: [&[u8; 4]; 3] = [b"avif", b"avis", b"avio"];

/// A few bytes can claim any canvas.
const MAX_PIXELS: u64 = 210_000_000;
const MAX_ITEMS: usize = 8192;

/// Whether `bytes` open as a HEIF file.
pub fn sniff(bytes: &[u8]) -> bool {
    let Some(([b'f', b't', b'y', b'p'], body)) = Boxes(bytes).next() else { return false };
    if body.len() < 8 {
        return false;
    }
    let brands = || std::iter::once(&body[..4]).chain(body[8..].chunks_exact(4));
    let any = |set: &[&[u8; 4]]| brands().any(|b| set.iter().any(|w| &w[..] == b));
    any(&HEVC) || any(&GENERIC) && !any(&AV1)
}

/// Whether the file carries a track as well as (or instead of) items: a sequence or burst.
pub fn has_track(bytes: &[u8]) -> bool {
    sniff(bytes) && Boxes(bytes).any(|(k, _)| &k == b"moov")
}

struct Boxes<'a>(&'a [u8]);

impl<'a> Iterator for Boxes<'a> {
    type Item = ([u8; 4], &'a [u8]);

    fn next(&mut self) -> Option<Self::Item> {
        let b = self.0;
        if b.len() < 8 {
            return None;
        }
        let kind = [b[4], b[5], b[6], b[7]];
        let (head, size) = match u32::from_be_bytes([b[0], b[1], b[2], b[3]]) {
            0 => (8, b.len() as u64),
            1 if b.len() >= 16 => (16, u64::from_be_bytes(b[8..16].try_into().ok()?)),
            1 => (16, u64::MAX),
            n => (8, u64::from(n)),
        };
        if size < head as u64 || size > b.len() as u64 {
            self.0 = &[];
            return None;
        }
        self.0 = &b[size as usize..];
        Some((kind, &b[head..size as usize]))
    }
}

struct Cur<'a>(&'a [u8]);

impl<'a> Cur<'a> {
    fn take(&mut self, n: usize) -> Option<&'a [u8]> {
        if self.0.len() < n {
            return None;
        }
        let (a, b) = self.0.split_at(n);
        self.0 = b;
        Some(a)
    }
    fn u8(&mut self) -> Option<u8> {
        Some(self.take(1)?[0])
    }
    fn u16(&mut self) -> Option<u16> {
        Some(u16::from_be_bytes(self.take(2)?.try_into().ok()?))
    }
    fn u32(&mut self) -> Option<u32> {
        Some(u32::from_be_bytes(self.take(4)?.try_into().ok()?))
    }
    fn u64(&mut self) -> Option<u64> {
        Some(u64::from_be_bytes(self.take(8)?.try_into().ok()?))
    }
    /// A FullBox header: its version.
    fn full(&mut self) -> Option<u8> {
        let v = self.u8()?;
        self.take(3)?;
        Some(v)
    }
    /// An `iloc` field `size` bytes wide.
    fn sized(&mut self, size: u8) -> Option<u64> {
        match size {
            0 => Some(0),
            4 => self.u32().map(u64::from),
            8 => self.u64(),
            _ => None,
        }
    }
    fn id(&mut self, wide: bool) -> Option<u32> {
        if wide { self.u32() } else { self.u16().map(u32::from) }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Nclx {
    pub matrix: u16,
    pub full: bool,
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Step {
    /// Degrees clockwise.
    Turn(u16),
    /// 0 mirrors top to bottom, 1 left to right, as libheif reads `imir`.
    Mirror(u8),
    /// Width, height, then the centre's offsets, each a numerator and denominator.
    Crop([u32; 8]),
}

#[derive(Clone, Debug)]
enum Prop<'a> {
    Hvcc(&'a [u8]),
    Size(u32, u32),
    Step(Step),
    Nclx(Nclx),
    Aux(&'a [u8]),
    Other,
}

struct Item {
    id: u32,
    kind: [u8; 4],
    /// Indices into `props`, in the order the file lists them.
    props: Vec<usize>,
    extents: Vec<(u64, u64)>,
    in_idat: bool,
}

struct Ref {
    kind: [u8; 4],
    from: u32,
    to: Vec<u32>,
}

pub struct Heif<'a> {
    file: &'a [u8],
    idat: &'a [u8],
    pub primary: u32,
    items: Vec<Item>,
    props: Vec<Prop<'a>>,
    refs: Vec<Ref>,
}

/// A picture as coded: one HEVC image or a grid of them, and how it turns and crops.
#[derive(Clone, Debug, PartialEq)]
pub struct Picture<'a> {
    pub size: (u32, u32),
    pub tile: (u32, u32),
    pub cols: u32,
    pub rows: u32,
    /// Row by row, each with its decoder configuration.
    pub tiles: Vec<(u32, &'a [u8])>,
    steps: Vec<Step>,
    pub nclx: Option<Nclx>,
}

fn parse_prop(kind: [u8; 4], body: &[u8]) -> Prop<'_> {
    let mut c = Cur(body);
    let mut p = || -> Option<Prop<'_>> {
        Some(match &kind {
            b"hvcC" => Prop::Hvcc(body),
            b"ispe" => {
                c.full()?;
                Prop::Size(c.u32()?, c.u32()?)
            }
            b"irot" => Prop::Step(Step::Turn((360 - u16::from(c.u8()? & 3) * 90) % 360)),
            b"imir" => Prop::Step(Step::Mirror(c.u8()? & 1)),
            b"clap" => {
                let mut v = [0u32; 8];
                for x in &mut v {
                    *x = c.u32()?;
                }
                Prop::Step(Step::Crop(v))
            }
            b"colr" if c.take(4)? == b"nclx" => {
                c.take(4)?;
                let matrix = c.u16()?;
                Prop::Nclx(Nclx { matrix, full: c.u8()? & 0x80 != 0 })
            }
            b"auxC" => {
                c.full()?;
                let rest = c.0;
                Prop::Aux(&rest[..rest.iter().position(|&b| b == 0).unwrap_or(rest.len())])
            }
            _ => Prop::Other,
        })
    };
    p().unwrap_or(Prop::Other)
}

impl<'a> Heif<'a> {
    pub fn parse(file: &'a [u8]) -> Option<Heif<'a>> {
        if !sniff(file) {
            return None;
        }
        let mut meta = Cur(Boxes(file).find(|(k, _)| k == b"meta")?.1);
        meta.full()?;
        let mut heif = Heif { file, idat: &[], primary: 0, items: Vec::new(), props: Vec::new(), refs: Vec::new() };
        let mut primary = None;
        let mut assoc: Vec<(u32, Vec<usize>)> = Vec::new();
        let mut locs: Vec<(u32, Vec<(u64, u64)>, bool)> = Vec::new();
        for (kind, body) in Boxes(meta.0) {
            let mut c = Cur(body);
            match &kind {
                b"pitm" => {
                    let v = c.full()?;
                    primary = Some(c.id(v != 0)?);
                }
                b"iinf" => {
                    let v = c.full()?;
                    c.id(v != 0)?;
                    for (k, infe) in Boxes(c.0).take(MAX_ITEMS) {
                        let mut c = Cur(infe);
                        let Some(v) = c.full().filter(|&v| &k == b"infe" && v >= 2) else { continue };
                        let (Some(id), Some(_), Some(t)) = (c.id(v != 2), c.u16(), c.take(4)) else { continue };
                        heif.items.push(Item { id, kind: [t[0], t[1], t[2], t[3]], props: Vec::new(), extents: Vec::new(), in_idat: false });
                    }
                }
                b"iloc" => {
                    let v = c.full()?;
                    let (a, b) = (c.u8()?, c.u8()?);
                    let (off, len, base) = (a >> 4, a & 15, b >> 4);
                    let index = if v >= 1 { b & 15 } else { 0 };
                    let count = if v < 2 { u32::from(c.u16()?) } else { c.u32()? };
                    for _ in 0..count.min(MAX_ITEMS as u32) {
                        let id = c.id(v >= 2)?;
                        let method = if v >= 1 { c.u16()? & 15 } else { 0 };
                        c.u16()?;
                        let base = c.sized(base)?;
                        let n = c.u16()?;
                        let mut extents = Vec::with_capacity(usize::from(n).min(64));
                        for _ in 0..n {
                            c.sized(index)?;
                            let o = c.sized(off)?;
                            let l = c.sized(len)?;
                            extents.push((base.checked_add(o)?, l));
                        }
                        if method <= 1 {
                            locs.push((id, extents, method == 1));
                        }
                    }
                }
                b"iprp" => {
                    for (k, b) in Boxes(body) {
                        match &k {
                            b"ipco" => heif.props.extend(Boxes(b).take(MAX_ITEMS).map(|(k, b)| parse_prop(k, b))),
                            b"ipma" => {
                                let mut c = Cur(b);
                                let v = c.u8()?;
                                let wide = c.take(3)?[2] & 1 != 0;
                                for _ in 0..c.u32()?.min(MAX_ITEMS as u32) {
                                    let id = c.id(v >= 1)?;
                                    let n = c.u8()?;
                                    let mut list = Vec::with_capacity(usize::from(n));
                                    for _ in 0..n {
                                        let i = if wide { usize::from(c.u16()? & 0x7FFF) } else { usize::from(c.u8()? & 0x7F) };
                                        if i > 0 {
                                            list.push(i - 1);
                                        }
                                    }
                                    assoc.push((id, list));
                                }
                            }
                            _ => {}
                        }
                    }
                }
                b"iref" => {
                    let v = c.full()?;
                    for (k, b) in Boxes(c.0).take(MAX_ITEMS) {
                        let mut c = Cur(b);
                        let Some(from) = c.id(v != 0) else { continue };
                        let Some(n) = c.u16() else { continue };
                        if let Some(to) = (0..n).map(|_| c.id(v != 0)).collect::<Option<Vec<u32>>>() {
                            heif.refs.push(Ref { kind: k, from, to });
                        }
                    }
                }
                b"idat" => heif.idat = body,
                _ => {}
            }
        }
        let props = heif.props.len();
        for (id, list) in assoc {
            if let Some(item) = heif.items.iter_mut().find(|i| i.id == id) {
                item.props.extend(list.into_iter().filter(|&i| i < props));
            }
        }
        for (id, extents, in_idat) in locs {
            if let Some(item) = heif.items.iter_mut().find(|i| i.id == id) {
                item.extents = extents;
                item.in_idat = in_idat;
            }
        }
        heif.primary = primary?;
        Some(heif)
    }

    fn item(&self, id: u32) -> Option<&Item> {
        self.items.iter().find(|i| i.id == id)
    }

    fn props(&self, item: &Item) -> impl Iterator<Item = &Prop<'a>> {
        item.props.iter().map(|&i| &self.props[i])
    }

    /// An item's bytes, joined from its extents.
    fn data(&self, item: &Item) -> Option<Cow<'a, [u8]>> {
        let src = if item.in_idat { self.idat } else { self.file };
        let piece = |&(off, len): &(u64, u64)| -> Option<&'a [u8]> {
            let rest = src.get(usize::try_from(off).ok()?..)?;
            if len == 0 { Some(rest) } else { rest.get(..usize::try_from(len).ok()?) }
        };
        match item.extents.as_slice() {
            [] => None,
            [one] => piece(one).map(Cow::Borrowed),
            many => {
                let mut out = Vec::new();
                for e in many {
                    let p = piece(e)?;
                    if out.len() + p.len() > src.len() {
                        return None;
                    }
                    out.extend_from_slice(p);
                }
                Some(Cow::Owned(out))
            }
        }
    }

    fn refs_from(&self, kind: &[u8; 4], from: u32) -> Option<&[u32]> {
        self.refs.iter().find(|r| &r.kind == kind && r.from == from).map(|r| r.to.as_slice())
    }

    /// Items referring to `to` with `kind`.
    fn refs_to<'s>(&'s self, kind: &'s [u8; 4], to: u32) -> impl Iterator<Item = u32> + 's {
        self.refs.iter().filter(move |r| &r.kind == kind && r.to.contains(&to)).map(|r| r.from)
    }

    fn size_of(&self, item: &Item) -> Option<(u32, u32)> {
        self.props(item).find_map(|p| if let Prop::Size(w, h) = p { Some((*w, *h)) } else { None })
    }

    fn hvcc_of(&self, item: &Item) -> Option<&'a [u8]> {
        self.props(item).find_map(|p| if let Prop::Hvcc(h) = p { Some(*h) } else { None })
    }

    fn nclx_of(&self, item: &Item) -> Option<Nclx> {
        self.props(item).find_map(|p| if let Prop::Nclx(n) = p { Some(*n) } else { None })
    }

    pub fn picture(&self, id: u32) -> Option<Picture<'a>> {
        let item = self.item(id)?;
        let steps = self.props(item).filter_map(|p| if let Prop::Step(s) = p { Some(*s) } else { None }).collect();
        let mut nclx = self.nclx_of(item);
        let (size, tile, cols, rows, tiles) = match &item.kind {
            b"hvc1" => {
                let size = self.size_of(item)?;
                (size, size, 1, 1, vec![(id, self.hvcc_of(item)?)])
            }
            b"grid" => {
                let data = self.data(item)?;
                let mut c = Cur(&data);
                c.u8()?;
                let wide = c.u8()? & 1 != 0;
                let rows = u32::from(c.u8()?) + 1;
                let cols = u32::from(c.u8()?) + 1;
                let size = if wide { (c.u32()?, c.u32()?) } else { (u32::from(c.u16()?), u32::from(c.u16()?)) };
                let ids = self.refs_from(b"dimg", id)?;
                if ids.len() != (rows * cols) as usize {
                    return None;
                }
                let first = self.item(ids[0])?;
                let tile = self.size_of(first)?;
                let covers = |t: u32, n: u32, s: u32| t > 0 && u64::from(t) * u64::from(n) >= u64::from(s);
                if !covers(tile.0, cols, size.0) || !covers(tile.1, rows, size.1) {
                    return None;
                }
                nclx = nclx.or_else(|| self.nclx_of(first));
                let tiles = ids
                    .iter()
                    .map(|&t| {
                        let it = self.item(t).filter(|it| &it.kind == b"hvc1")?;
                        (self.size_of(it)? == tile).then_some(())?;
                        Some((t, self.hvcc_of(it)?))
                    })
                    .collect::<Option<Vec<_>>>()?;
                (size, tile, cols, rows, tiles)
            }
            _ => return None,
        };
        if size.0 == 0 || size.1 == 0 || u64::from(size.0) * u64::from(size.1) > MAX_PIXELS {
            return None;
        }
        Some(Picture { size, tile, cols, rows, tiles, steps, nclx })
    }

    pub fn overlay(&self, id: u32) -> Option<Overlay> {
        let item = self.item(id).filter(|i| &i.kind == b"iovl")?;
        let data = self.data(item)?;
        let mut c = Cur(&data);
        c.u8()?;
        let wide = c.u8()? & 1 != 0;
        let fill = [c.u16()?, c.u16()?, c.u16()?, c.u16()?];
        let field = |c: &mut Cur| if wide { c.u32() } else { c.u16().map(u32::from) };
        let size = (field(&mut c)?, field(&mut c)?);
        if size.0 == 0 || size.1 == 0 || u64::from(size.0) * u64::from(size.1) > MAX_PIXELS {
            return None;
        }
        let offset = |c: &mut Cur| if wide { c.u32().map(|v| v as i32) } else { c.u16().map(|v| i32::from(v as i16)) };
        let layers = self
            .refs_from(b"dimg", id)?
            .iter()
            .map(|&l| Some((l, offset(&mut c)?, offset(&mut c)?)))
            .collect::<Option<Vec<_>>>()?;
        let steps = self.props(item).filter_map(|p| if let Prop::Step(s) = p { Some(*s) } else { None }).collect();
        Some(Overlay { size, fill, layers, steps })
    }

    /// The size picture `id` shows at, an overlay's included.
    pub fn shown(&self, id: u32) -> Option<(u32, u32)> {
        match self.picture(id) {
            Some(p) => Some(p.geometry().shown()),
            None => Some(self.overlay(id)?.geometry().shown()),
        }
    }

    /// The primary picture's thumbnails.
    pub fn thumbnails(&self) -> impl Iterator<Item = u32> + '_ {
        self.refs_to(b"thmb", self.primary)
    }

    /// The alpha picture for `id`, and whether `id`'s colour is premultiplied by it.
    pub fn alpha_of(&self, id: u32) -> Option<(u32, bool)> {
        let alpha = self.refs_to(b"auxl", id).find(|&a| {
            self.item(a).is_some_and(|it| {
                self.props(it).any(|p| {
                    matches!(p, Prop::Aux(t) if *t == b"urn:mpeg:hevc:2015:auxid:1" || *t == b"urn:mpeg:mpegB:cicp:systems:auxiliary:alpha")
                })
            })
        })?;
        Some((alpha, self.refs_from(b"prem", id).is_some_and(|to| to.contains(&alpha))))
    }
}

/// Pictures placed on a filled canvas in order (`iovl`).
pub struct Overlay {
    pub size: (u32, u32),
    /// RGBA, 16 bits a channel.
    pub fill: [u16; 4],
    /// Each picture and where its top left sits.
    pub layers: Vec<(u32, i32, i32)>,
    steps: Vec<Step>,
}

impl Overlay {
    pub fn geometry(&self) -> Geometry {
        geometry(self.size, &self.steps)
    }
}

/// Where the shown picture sits in the decoded one, and how it turns: `rot` degrees clockwise,
/// then mirrored left to right when `flip`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Geometry {
    pub x: u32,
    pub y: u32,
    pub w: u32,
    pub h: u32,
    pub rot: u16,
    pub flip: bool,
}

impl Geometry {
    pub fn shown(&self) -> (u32, u32) {
        if self.rot % 180 == 90 { (self.h, self.w) } else { (self.w, self.h) }
    }
}

impl Picture<'_> {
    pub fn geometry(&self) -> Geometry {
        geometry(self.size, &self.steps)
    }
}

fn geometry(size: (u32, u32), steps: &[Step]) -> Geometry {
    let mut g = Geometry { x: 0, y: 0, w: size.0, h: size.1, rot: 0, flip: false };
    for step in steps {
        match *step {
            // Turning a mirrored picture clockwise turns what was decoded the other way.
            Step::Turn(r) => g.rot = if g.flip { (g.rot + 360 - r) % 360 } else { (g.rot + r) % 360 },
            Step::Mirror(1) => g.flip = !g.flip,
            // Top to bottom is left to right and half a turn.
            Step::Mirror(_) => {
                g.rot = (g.rot + 180) % 360;
                g.flip = !g.flip;
            }
            Step::Crop(c) => {
                if let Some(r) = crop(g, c) {
                    g = r;
                }
            }
        }
    }
    g
}

/// `clap` applied to what `g` shows, mapped back into the decoded picture.
fn crop(g: Geometry, c: [u32; 8]) -> Option<Geometry> {
    let ratio = |n: f64, d: u32| (d != 0).then(|| n / f64::from(d));
    let (sw, sh) = g.shown();
    let (sw, sh) = (f64::from(sw), f64::from(sh));
    let cw = ratio(f64::from(c[0]), c[1])?.round();
    let ch = ratio(f64::from(c[2]), c[3])?.round();
    let left = (ratio(f64::from(c[4] as i32), c[5])? + (sw - 1.) / 2. - (cw - 1.) / 2.).round();
    let top = (ratio(f64::from(c[6] as i32), c[7])? + (sh - 1.) / 2. - (ch - 1.) / 2.).round();
    let (l, t) = (left.clamp(0., sw), top.clamp(0., sh));
    let (r, b) = ((left + cw).clamp(l, sw), (top + ch).clamp(t, sh));
    if r - l < 1. || b - t < 1. {
        return None;
    }
    // Corners of what is shown, back through the mirror and the turn.
    let back = |x: f64, y: f64| -> (f64, f64) {
        let x = if g.flip { sw - x } else { x };
        let (w, h) = (f64::from(g.w), f64::from(g.h));
        match g.rot {
            90 => (y, h - x),
            180 => (w - x, h - y),
            270 => (w - y, x),
            _ => (x, y),
        }
    };
    let (a, z) = (back(l, t), back(r, b));
    let (x0, y0) = (a.0.min(z.0), a.1.min(z.1));
    Some(Geometry {
        x: g.x + x0 as u32,
        y: g.y + y0 as u32,
        w: (a.0.max(z.0) - x0) as u32,
        h: (a.1.max(z.1) - y0) as u32,
        ..g
    })
}

/// The largest size within `max` keeping the aspect, never larger than `size`; rounded as
/// `DynamicImage::resize` rounds, as the other pictures are.
pub fn fit(size: (u32, u32), max: (u32, u32)) -> (u32, u32) {
    if size.0 <= max.0 && size.1 <= max.1 {
        return size;
    }
    let r = (f64::from(max.0.max(1)) / f64::from(size.0)).min(f64::from(max.1.max(1)) / f64::from(size.1));
    (((f64::from(size.0) * r).round() as u32).max(1), ((f64::from(size.1) * r).round() as u32).max(1))
}

/// YCbCr samples at one bit depth to RGB in 0..=255.
#[derive(Clone, Copy, Debug)]
pub struct Matrix {
    ys: f32,
    yo: f32,
    cs: f32,
    co: f32,
    crv: f32,
    cgu: f32,
    cgv: f32,
    cbu: f32,
    identity: bool,
}

impl Matrix {
    /// The file's `nclx`, else what the stream says, else BT.601 full range as libheif assumes.
    pub fn new(file: Option<Nclx>, stream: (Option<u16>, Option<bool>), depth: u8) -> Matrix {
        let matrix = file.map(|n| n.matrix).filter(|&m| m != 2).or(stream.0).unwrap_or(6);
        let full = file.map(|n| n.full).or(stream.1).unwrap_or(true);
        let max = ((1u32 << depth) - 1) as f32;
        let unit = (1u32 << (depth - 8)) as f32;
        let (ys, yo, cs, co) = if full {
            (1. / max, 0., 1. / max, (1u32 << (depth - 1)) as f32 / max)
        } else {
            (1. / (219. * unit), 16. / 219., 1. / (224. * unit), 128. / 224.)
        };
        let (kr, kb) = match matrix {
            1 => (0.2126, 0.0722),
            4 => (0.30, 0.11),
            7 => (0.212, 0.087),
            9 | 10 => (0.2627, 0.0593),
            _ => (0.299, 0.114),
        };
        let kg = 1. - kr - kb;
        Matrix {
            ys: ys * 255.,
            yo: yo * 255.,
            cs,
            co,
            crv: 2. * (1. - kr) * 255.,
            cgu: 2. * kb * (1. - kb) / kg * 255.,
            cgv: 2. * kr * (1. - kr) / kg * 255.,
            cbu: 2. * (1. - kb) * 255.,
            identity: matrix == 0,
        }
    }

    /// `c` is None for grey.
    #[inline(always)]
    pub fn rgb(&self, y: f32, c: Option<(f32, f32)>) -> [f32; 3] {
        let l = y * self.ys - self.yo;
        let Some((u, v)) = c else { return [l, l, l] };
        if self.identity {
            return [v * self.ys - self.yo, l, u * self.ys - self.yo];
        }
        let (u, v) = (u * self.cs - self.co, v * self.cs - self.co);
        [l + self.crv * v, l - self.cgu * u - self.cgv * v, l + self.cbu * u]
    }
}

fn catmull_rom(x: f32) -> f32 {
    let a = x.abs();
    if a < 1. {
        (9. * a * a * a - 15. * a * a + 6.) / 6.
    } else if a < 2. {
        (-3. * a * a * a + 15. * a * a - 24. * a + 12.) / 6.
    } else {
        0.
    }
}

/// A filter along one axis: per output index, its first source index and weights.
pub struct Taps {
    spans: Vec<(u32, u32, u32)>,
    weights: Vec<f32>,
}

impl Taps {
    pub fn new(len: usize, new_len: usize) -> Taps {
        if len == new_len {
            return Taps { spans: (0..len as u32).map(|i| (i, 1, i)).collect(), weights: vec![1.; len] };
        }
        let ratio = len as f32 / new_len as f32;
        let scale = ratio.max(1.);
        let support = 2. * scale;
        let mut spans = Vec::with_capacity(new_len);
        let mut weights = Vec::with_capacity(new_len * (2. * support).ceil() as usize + new_len);
        for out in 0..new_len {
            let centre = (out as f32 + 0.5) * ratio;
            let left = ((centre - support).floor().max(0.) as usize).min(len - 1);
            let right = ((centre + support).ceil() as usize).clamp(left + 1, len);
            let at = weights.len();
            let mut sum = 0.;
            for i in left..right {
                let w = catmull_rom((i as f32 + 0.5 - centre) / scale);
                weights.push(w);
                sum += w;
            }
            if sum != 0. {
                weights[at..].iter_mut().for_each(|w| *w /= sum);
            }
            spans.push((left as u32, (right - left) as u32, at as u32));
        }
        Taps { spans, weights }
    }

    #[inline(always)]
    fn get(&self, i: usize) -> (usize, &[f32]) {
        let (left, n, at) = self.spans[i];
        (left as usize, &self.weights[at as usize..(at + n) as usize])
    }

    /// One row of RGBA filtered into `to`.
    pub fn apply(&self, row: &[f32], to: &mut [f32]) {
        for (o, px) in to.chunks_exact_mut(4).enumerate() {
            let (left, ws) = self.get(o);
            let mut t = [0f32; 4];
            for (s, &w) in row[left * 4..].chunks_exact(4).zip(ws) {
                for c in 0..4 {
                    t[c] += s[c] * w;
                }
            }
            px.copy_from_slice(&t);
        }
    }
}

/// Rows already filtered across, added into the output rows they reach as they arrive, so only
/// the output rows still open are held.
pub struct Down {
    nw: usize,
    nh: usize,
    taps: Taps,
    next: usize,
    open: std::collections::VecDeque<Vec<f32>>,
    spare: Vec<Vec<f32>>,
    pub out: Vec<u8>,
}

impl Down {
    pub fn new(h: usize, dst: (usize, usize)) -> Down {
        Down {
            nw: dst.0,
            nh: dst.1,
            taps: Taps::new(h, dst.1),
            next: 0,
            open: Default::default(),
            spare: Vec::new(),
            out: vec![0; dst.0 * dst.1 * 4],
        }
    }

    /// Source row `y`; rows come in order.
    pub fn push(&mut self, y: usize, row: &[f32]) {
        for o in self.next..self.nh {
            let (left, ws) = self.taps.get(o);
            if left > y {
                break;
            }
            let Some(&w) = ws.get(y - left) else { continue };
            let i = o - self.next;
            while self.open.len() <= i {
                let mut v = self.spare.pop().unwrap_or_default();
                v.clear();
                v.resize(self.nw * 4, 0.);
                self.open.push_back(v);
            }
            for (a, &s) in self.open[i].iter_mut().zip(row) {
                *a += s * w;
            }
        }
        while self.next < self.nh {
            let (left, ws) = self.taps.get(self.next);
            if left + ws.len() > y + 1 {
                break;
            }
            let acc = self.open.pop_front().unwrap_or_else(|| vec![0.; self.nw * 4]);
            let line = &mut self.out[self.next * self.nw * 4..][..self.nw * 4];
            for (d, &a) in line.iter_mut().zip(&acc) {
                *d = a.clamp(0., 255.).round() as u8;
            }
            self.spare.push(acc);
            self.next += 1;
        }
    }

    pub fn done(&self) -> bool {
        self.next == self.nh
    }
}

/// Scaled RGBA as decoded (`w`×`h`), turned and mirrored as shown, into straight BGRA.
pub fn orient(rgba: &[u8], w: usize, h: usize, g: &Geometry, premultiplied: bool) -> image::RgbaImage {
    let (ow, oh) = if g.rot % 180 == 90 { (h, w) } else { (w, h) };
    let mut out = vec![0u8; ow * oh * 4];
    for (y, line) in out.chunks_exact_mut(ow * 4).enumerate() {
        for (x, px) in line.chunks_exact_mut(4).enumerate() {
            let x = if g.flip { ow - 1 - x } else { x };
            let (sx, sy) = match g.rot {
                90 => (y, h - 1 - x),
                180 => (w - 1 - x, h - 1 - y),
                270 => (w - 1 - y, x),
                _ => (x, y),
            };
            let s = &rgba[(sy * w + sx) * 4..][..4];
            let a = u32::from(s[3]);
            let un = |v: u8| {
                if premultiplied && a > 0 && a < 255 { ((u32::from(v) * 255 + a / 2) / a).min(255) as u8 } else { v }
            };
            px.copy_from_slice(&[un(s[2]), un(s[1]), un(s[0]), s[3]]);
        }
    }
    image::RgbaImage::from_raw(ow as u32, oh as u32, out).expect("sized to fit")
}

#[cfg(feature = "video")]
pub use decode::decode;

/// Built without FFmpeg: HEIF draws as a file.
#[cfg(not(feature = "video"))]
pub fn decode(_: &[u8], _: (u32, u32)) -> Option<(image::RgbaImage, u32, u32)> {
    None
}

#[cfg(feature = "video")]
mod decode {
    use super::*;
    use ffmpeg_next as ffmpeg;
    use rayon::prelude::*;

    /// A tile as FFmpeg decoded it.
    struct Tile {
        frame: ffmpeg::frame::Video,
        depth: u8,
        /// Chroma subsampling as shifts; None for grey.
        shift: Option<(u8, u8)>,
    }

    impl Tile {
        fn new(frame: ffmpeg::frame::Video) -> Option<Tile> {
            use ffmpeg::format::Pixel as P;
            let (depth, shift) = match frame.format() {
                P::YUV420P | P::YUVJ420P => (8, Some((1, 1))),
                P::YUV422P | P::YUVJ422P => (8, Some((1, 0))),
                P::YUV444P | P::YUVJ444P => (8, Some((0, 0))),
                P::GRAY8 => (8, None),
                P::YUV420P10LE => (10, Some((1, 1))),
                P::YUV422P10LE => (10, Some((1, 0))),
                P::YUV444P10LE => (10, Some((0, 0))),
                P::GRAY10LE => (10, None),
                P::YUV420P12LE => (12, Some((1, 1))),
                P::YUV422P12LE => (12, Some((1, 0))),
                P::YUV444P12LE => (12, Some((0, 0))),
                P::GRAY12LE => (12, None),
                _ => return None,
            };
            Some(Tile { frame, depth, shift })
        }

        /// The stream's own matrix and range, where it names them.
        fn signalled(&self) -> (Option<u16>, Option<bool>) {
            use ffmpeg::color::{Range as R, Space as S};
            let matrix = match self.frame.color_space() {
                S::RGB => Some(0),
                S::BT709 => Some(1),
                S::FCC => Some(4),
                S::BT470BG => Some(5),
                S::SMPTE170M => Some(6),
                S::SMPTE240M => Some(7),
                S::BT2020NCL => Some(9),
                S::BT2020CL => Some(10),
                _ => None,
            };
            let full = match self.frame.color_range() {
                R::JPEG => Some(true),
                R::MPEG => Some(false),
                _ => None,
            };
            (matrix, full)
        }
    }

    #[derive(Clone, Copy)]
    struct Plane<'f> {
        data: &'f [u8],
        stride: usize,
    }

    impl Plane<'_> {
        /// A sample, one byte or two little-endian.
        #[inline(always)]
        fn at(&self, wide: bool, mask: u16, x: usize, y: usize) -> f32 {
            if wide {
                let i = y * self.stride + x * 2;
                f32::from(u16::from_le_bytes([self.data[i], self.data[i + 1]]) & mask)
            } else {
                f32::from(self.data[y * self.stride + x])
            }
        }
    }

    /// One HEVC configuration's decoder.
    struct Hevc {
        hvcc: Vec<u8>,
        dec: ffmpeg::decoder::Video,
        drained: bool,
    }

    impl Hevc {
        fn open(hvcc: &[u8]) -> Option<Hevc> {
            if !crate::video::ffmpeg_ready() {
                return None;
            }
            let codec = ffmpeg::decoder::find(ffmpeg::codec::Id::HEVC)?;
            let mut ctx = ffmpeg::codec::context::Context::new_with_codec(codec);
            // SAFETY: extradata is FFmpeg-allocated with its padding, and freed with the context.
            unsafe {
                let pad = ffmpeg::ffi::AV_INPUT_BUFFER_PADDING_SIZE as usize;
                let buf = ffmpeg::ffi::av_mallocz(hvcc.len() + pad) as *mut u8;
                if buf.is_null() {
                    return None;
                }
                std::ptr::copy_nonoverlapping(hvcc.as_ptr(), buf, hvcc.len());
                let p = ctx.as_mut_ptr();
                (*p).extradata = buf;
                (*p).extradata_size = hvcc.len() as i32;
            }
            // Each frame thread holds a tile of its own.
            let threads = std::thread::available_parallelism().map_or(2, |n| n.get()).clamp(1, 4);
            ctx.set_threading(ffmpeg::threading::Config { kind: ffmpeg::threading::Type::Frame, count: threads });
            Some(Hevc { hvcc: hvcc.to_vec(), dec: ctx.decoder().video().ok()?, drained: false })
        }
    }

    /// Tiles out in the order asked, sent ahead so the decoder's threads stay busy.
    struct Feed<'h, 'a> {
        heif: &'h Heif<'a>,
        order: Vec<(u32, &'a [u8])>,
        sent: usize,
        decoders: Vec<Hevc>,
        /// The decoder each tile in flight went to.
        routed: std::collections::VecDeque<usize>,
    }

    impl<'h, 'a> Feed<'h, 'a> {
        fn new(heif: &'h Heif<'a>, order: Vec<(u32, &'a [u8])>) -> Self {
            Feed { heif, order, sent: 0, decoders: Vec::new(), routed: Default::default() }
        }

        fn next(&mut self) -> Option<Tile> {
            loop {
                let front = self.routed.front().copied().unwrap_or(usize::MAX);
                if let Some(h) = self.decoders.get_mut(front) {
                    let mut frame = ffmpeg::frame::Video::empty();
                    if h.dec.receive_frame(&mut frame).is_ok() {
                        self.routed.pop_front();
                        return Tile::new(frame);
                    }
                }
                if let Some(&(id, hvcc)) = self.order.get(self.sent) {
                    self.sent += 1;
                    let d = match self.decoders.iter().position(|h| h.hvcc == hvcc) {
                        Some(d) => d,
                        None => {
                            self.decoders.push(Hevc::open(hvcc)?);
                            self.decoders.len() - 1
                        }
                    };
                    let data = self.heif.data(self.heif.item(id)?)?;
                    self.decoders[d].dec.send_packet(&ffmpeg::Packet::copy(&data)).ok()?;
                    self.routed.push_back(d);
                } else {
                    let h = self.decoders.get_mut(front)?;
                    if std::mem::replace(&mut h.drained, true) {
                        return None;
                    }
                    h.dec.send_eof().ok()?;
                }
            }
        }
    }

    /// What `rows` makes of each pixel.
    #[derive(Clone, Copy)]
    enum Out<'p> {
        /// RGBA, premultiplied by `alpha` (one byte a pixel of the cropped picture) when given.
        Colour { alpha: Option<&'p [u8]> },
        /// Luma alone, as 0..=255 in every channel.
        Luma,
    }

    /// Every row of `pic` inside `g` converted, filtered across by `across` and handed to `row`
    /// in order with its index from the top of `g`, decoding a row of tiles at a time.
    fn rows(heif: &Heif, pic: &Picture, g: &Geometry, out: Out, across: &Taps, nw: usize, row: &mut dyn FnMut(usize, &[f32])) -> Option<()> {
        let (tw, th) = (pic.tile.0 as usize, pic.tile.1 as usize);
        let (x0, x1) = (g.x as usize, (g.x + g.w) as usize);
        let (y0, y1) = (g.y as usize, (g.y + g.h) as usize);
        let (c0, c1) = (x0 / tw, x1.div_ceil(tw));
        let (r0, r1) = (y0 / th, y1.div_ceil(th));
        let order = (r0..r1)
            .flat_map(|r| (c0..c1).map(move |c| (r, c)))
            .map(|(r, c)| pic.tiles.get(r * pic.cols as usize + c).copied())
            .collect::<Option<Vec<_>>>()?;
        let mut feed = Feed::new(heif, order);
        let mut matrix = None;
        // The tile row above's last chroma row, for the luma rows that straddle the seam.
        let mut carry: Vec<(f32, f32)> = Vec::new();
        let mut lines: Vec<f32> = Vec::new();
        let w = x1 - x0;
        for r in r0..r1 {
            let band: Vec<Tile> = (c0..c1).map(|_| feed.next()).collect::<Option<_>>()?;
            let first = &band[0];
            if band.iter().any(|t| {
                t.depth != first.depth || t.shift != first.shift || (t.frame.width() as usize) < tw || (t.frame.height() as usize) < th
            }) {
                return None;
            }
            let m = *matrix.get_or_insert_with(|| Matrix::new(pic.nclx, first.signalled(), first.depth));
            let (wide, mask) = (first.depth > 8, (1u16 << first.depth) - 1);
            let luma_max = f32::from(mask);
            // Each tile's planes, looked up once: FFmpeg sizes a plane from its format on every ask.
            let planes: Vec<[Plane; 3]> = band
                .iter()
                .map(|t| {
                    let n = t.frame.planes().min(3);
                    std::array::from_fn(|i| if i < n { Plane { data: t.frame.data(i), stride: t.frame.stride(i) } } else { Plane { data: &[], stride: 0 } })
                })
                .collect();
            let (by0, by1) = ((r * th).max(y0), ((r + 1) * th).min(y1));
            let (sx, sy) = first.shift.unwrap_or((0, 0));
            let (ctw, cth) = (tw.div_ceil(1 << sx), th.div_ceil(1 << sy));
            let cw = band.len() * ctw;
            // A chroma row across every tile of the band.
            let chroma_row = |cy: usize, out: &mut Vec<(f32, f32)>| {
                out.extend((0..cw).map(|i| {
                    let p = &planes[i / ctw];
                    (p[1].at(wide, mask, i % ctw, cy), p[2].at(wide, mask, i % ctw, cy))
                }))
            };
            let colour = matches!(out, Out::Colour { .. }) && first.shift.is_some();
            let lo = (by0 - r * th) >> sy;
            let hi = (((by1 - 1 - r * th) >> sy) + 2).min(cth);
            let mut chroma = Vec::new();
            if colour {
                chroma.reserve_exact((hi - lo) * cw);
                (lo..hi).for_each(|cy| chroma_row(cy, &mut chroma));
            }
            let chroma_at = |j: usize| chroma.get((j - lo) * cw..(j - lo + 1) * cw);
            lines.resize((by1 - by0) * nw * 4, 0.);
            lines.par_chunks_mut(nw * 4).enumerate().for_each_init(
                || vec![0f32; w * 4],
                |line, (k, to)| {
                    let y = by0 + k;
                    let ly = y - r * th;
                    // Vertically centred chroma: three parts its own row to one of the nearer
                    // neighbour.
                    let (here, other) = if colour {
                        let j = ly >> sy;
                        let here = chroma_at(j).unwrap_or(&[]);
                        let other = match (sy, ly % 2) {
                            (0, _) => None,
                            (_, 0) if j > lo => chroma_at(j - 1),
                            (_, 0) => Some(carry.as_slice()).filter(|c| !c.is_empty()),
                            _ => chroma_at(j + 1),
                        };
                        (here, other.unwrap_or(here))
                    } else {
                        (&[][..], &[][..])
                    };
                    let mix = |i: usize| {
                        let (a, b) = (here[i], other[i]);
                        (0.75 * a.0 + 0.25 * b.0, 0.75 * a.1 + 0.25 * b.1)
                    };
                    for (i, px) in line.chunks_exact_mut(4).enumerate() {
                        let x = x0 + i - c0 * tw;
                        let (t, lx) = (x / tw, x % tw);
                        let luma = planes[t][0].at(wide, mask, lx, ly);
                        let rgba = match out {
                            Out::Luma => {
                                let v = luma * 255. / luma_max;
                                [v, v, v, 255.]
                            }
                            Out::Colour { alpha } => {
                                // Horizontally co-sited: odd columns sit between two samples.
                                let c = (!here.is_empty()).then(|| {
                                    let ci = t * ctw + (lx >> sx);
                                    if sx == 0 || lx % 2 == 0 {
                                        mix(ci)
                                    } else {
                                        let (a, b) = (mix(ci), mix((ci + 1).min(cw - 1)));
                                        ((a.0 + b.0) / 2., (a.1 + b.1) / 2.)
                                    }
                                });
                                let [r, g, b] = m.rgb(luma, c).map(|v| v.clamp(0., 255.));
                                match alpha {
                                    Some(a) => {
                                        let a = f32::from(a[(y - y0) * w + i]);
                                        let k = a / 255.;
                                        [r * k, g * k, b * k, a]
                                    }
                                    None => [r, g, b, 255.],
                                }
                            }
                        };
                        px.copy_from_slice(&rgba);
                    }
                    across.apply(line, to);
                },
            );
            for (k, line) in lines.chunks_exact(nw * 4).enumerate() {
                row(by0 + k - y0, line);
            }
            if colour && sy > 0 {
                carry.clear();
                chroma_row(cth - 1, &mut carry);
            }
        }
        Some(())
    }

    /// An alpha picture's plane over `g` of a picture `size` big, one byte a pixel.
    fn alpha_plane(heif: &Heif, id: u32, size: (u32, u32), g: &Geometry) -> Option<Vec<u8>> {
        let pic = heif.picture(id).filter(|p| p.size == size)?;
        let w = g.w as usize;
        let mut plane = vec![0u8; w * g.h as usize];
        rows(heif, &pic, g, Out::Luma, &Taps::new(w, w), w, &mut |y, line| {
            for (d, px) in plane[y * w..][..w].iter_mut().zip(line.chunks_exact(4)) {
                *d = px[0].round().clamp(0., 255.) as u8;
            }
        })?;
        Some(plane)
    }

    /// Picture `id` no larger than `max` as shown, in straight BGRA.
    fn still(heif: &Heif, id: u32, max: (u32, u32)) -> Option<image::RgbaImage> {
        let pic = heif.picture(id)?;
        let g = pic.geometry();
        let shown = fit(g.shown(), max);
        let (pw, ph) = if g.rot % 180 == 90 { (shown.1 as usize, shown.0 as usize) } else { (shown.0 as usize, shown.1 as usize) };
        let alpha = heif.alpha_of(id);
        let plane = alpha.and_then(|(a, _)| alpha_plane(heif, a, pic.size, &g));
        // Premultiplied colour scales without dark fringes; a file that stored it so is used as is.
        let stored = alpha.is_some_and(|(_, p)| p);
        let out = Out::Colour { alpha: plane.as_deref().filter(|_| !stored) };
        let across = Taps::new(g.w as usize, pw);
        let mut down = Down::new(g.h as usize, (pw, ph));
        rows(heif, &pic, &g, out, &across, pw, &mut |y, line| down.push(y, line))?;
        down.done().then_some(())?;
        Some(orient(&down.out, pw, ph, &g, plane.is_some()))
    }

    /// An overlay no larger than `max` as shown, in straight BGRA: the canvas built at the
    /// scale it shows at, each picture decoded straight to its scaled size.
    fn overlaid(heif: &Heif, ov: &Overlay, max: (u32, u32)) -> Option<image::RgbaImage> {
        let g = ov.geometry();
        let shown = fit(g.shown(), max);
        let s = f64::from(shown.0) / f64::from(g.shown().0);
        let scaled = |v: f64| (v * s).round();
        let (cw, ch) = (scaled(f64::from(ov.size.0)).max(1.) as usize, scaled(f64::from(ov.size.1)).max(1.) as usize);
        let fill = ov.fill.map(|v| (v >> 8) as u8);
        let mut canvas = fill.repeat(cw * ch);
        for &(id, x, y) in &ov.layers {
            let Some(natural) = heif.picture(id).map(|p| p.geometry().shown()) else { continue };
            let size = (scaled(f64::from(natural.0)).max(1.) as u32, scaled(f64::from(natural.1)).max(1.) as u32);
            let Some(mut img) = still(heif, id, size) else { continue };
            if (img.width(), img.height()) != size {
                img = gpui::resample(&img, size.0, size.1, image::imageops::FilterType::CatmullRom);
            }
            let (x, y) = (scaled(f64::from(x)) as i64, scaled(f64::from(y)) as i64);
            for (row, line) in img.as_raw().chunks_exact(size.0 as usize * 4).enumerate() {
                let cy = y + row as i64;
                if cy < 0 || cy >= ch as i64 {
                    continue;
                }
                for (col, px) in line.chunks_exact(4).enumerate() {
                    let cx = x + col as i64;
                    if cx < 0 || cx >= cw as i64 {
                        continue;
                    }
                    let d = &mut canvas[(cy as usize * cw + cx as usize) * 4..][..4];
                    let (sa, da) = (f32::from(px[3]) / 255., f32::from(d[3]) / 255.);
                    let a = sa + da * (1. - sa);
                    let src = [px[2], px[1], px[0]];
                    for c in 0..3 {
                        let v = if a > 0. { (f32::from(src[c]) * sa + f32::from(d[c]) * da * (1. - sa)) / a } else { 0. };
                        d[c] = v.round().clamp(0., 255.) as u8;
                    }
                    d[3] = (a * 255.).round() as u8;
                }
            }
        }
        // The overlay's own crop, at the canvas's scale, then its turn and mirror.
        let (x0, y0) = ((scaled(f64::from(g.x)) as usize).min(cw - 1), (scaled(f64::from(g.y)) as usize).min(ch - 1));
        let (w, h) = if g.rot % 180 == 90 { (shown.1 as usize, shown.0 as usize) } else { (shown.0 as usize, shown.1 as usize) };
        let (w, h) = (w.min(cw - x0), h.min(ch - y0));
        let mut cut = Vec::with_capacity(w * h * 4);
        for row in canvas.chunks_exact(cw * 4).skip(y0).take(h) {
            cut.extend_from_slice(&row[x0 * 4..(x0 + w) * 4]);
        }
        Some(orient(&cut, w, h, &g, false))
    }

    /// The primary picture no larger than `max`, as straight BGRA, with its natural size.
    pub fn decode(bytes: &[u8], max: (u32, u32)) -> Option<(image::RgbaImage, u32, u32)> {
        let heif = Heif::parse(bytes)?;
        let natural = heif.shown(heif.primary)?;
        let want = fit(natural, max);
        // A thumbnail the same shape and at least as large does the job for less.
        let (nw, nh) = (u64::from(natural.0), u64::from(natural.1));
        let thumb = heif
            .thumbnails()
            .filter_map(|t| Some((t, heif.picture(t)?.geometry().shown())))
            .filter(|&(_, (w, h))| {
                let (w, h) = (u64::from(w), u64::from(h));
                w >= u64::from(want.0) && h >= u64::from(want.1) && (w * nh).abs_diff(h * nw) * 50 <= w * nh
            })
            .min_by_key(|&(_, (w, h))| u64::from(w) * u64::from(h));
        let primary = || match heif.overlay(heif.primary) {
            Some(ov) => overlaid(&heif, &ov, want),
            None => still(&heif, heif.primary, want),
        };
        let img = thumb.and_then(|(t, _)| still(&heif, t, want)).or_else(primary)?;
        let img = if (img.width(), img.height()) == want {
            img
        } else {
            gpui::resample(&img, want.0, want.1, image::imageops::FilterType::CatmullRom)
        };
        Some((img, natural.0, natural.1))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bx(kind: &[u8; 4], body: &[u8]) -> Vec<u8> {
        let mut v = ((body.len() + 8) as u32).to_be_bytes().to_vec();
        v.extend_from_slice(kind);
        v.extend_from_slice(body);
        v
    }

    fn pic(size: (u32, u32), steps: Vec<Step>) -> Picture<'static> {
        Picture { size, tile: size, cols: 1, rows: 1, tiles: Vec::new(), steps, nclx: None }
    }

    #[test]
    fn sniffs_heif_brands_only() {
        let ftyp = |major: &[u8; 4], compat: &[u8]| {
            let mut b = major.to_vec();
            b.extend_from_slice(&[0, 0, 0, 0]);
            b.extend_from_slice(compat);
            bx(b"ftyp", &b)
        };
        assert!(sniff(&ftyp(b"heic", b"mif1")));
        assert!(sniff(&ftyp(b"mif1", b"heic")));
        assert!(sniff(&ftyp(b"isom", b"msf1hevc")));
        assert!(!sniff(&ftyp(b"isom", b"mp41avc1")));
        assert!(!sniff(&ftyp(b"avif", b"mif1miaf")));
        assert!(!sniff(b"\x00\x00\x00\x08ftyp"));
        assert!(!sniff(&[]));
    }

    #[test]
    fn transforms_compose_in_file_order() {
        // A quarter turn anticlockwise shows the picture on its side.
        let g = pic((40, 30), vec![Step::Turn(270)]).geometry();
        assert_eq!((g.rot, g.flip, g.shown()), (270, false, (30, 40)));
        // Turning then mirroring top to bottom: half a turn more, mirrored.
        let g = pic((40, 30), vec![Step::Turn(90), Step::Mirror(0)]).geometry();
        assert_eq!((g.rot, g.flip), (270, true));
        // Mirrored first, a later turn goes the other way.
        let g = pic((40, 30), vec![Step::Mirror(1), Step::Turn(90)]).geometry();
        assert_eq!((g.rot, g.flip), (270, true));
    }

    #[test]
    fn a_crop_after_a_turn_maps_back_into_the_decoded_picture() {
        // 40×30 turned a quarter clockwise shows 30×40; keep the top 30×20 of what is shown.
        let crop = Step::Crop([30, 1, 20, 1, 0, 1, (-10i32) as u32, 1]);
        let g = pic((40, 30), vec![Step::Turn(90), crop]).geometry();
        assert_eq!(g.shown(), (30, 20));
        // The top of what is shown is the decoded picture's left side.
        assert_eq!((g.x, g.y, g.w, g.h), (0, 0, 20, 30));
        let g = pic((40, 30), vec![Step::Turn(270), crop]).geometry();
        assert_eq!((g.x, g.y, g.w, g.h), (20, 0, 20, 30));
    }

    #[test]
    fn a_centred_crop_rounds_half_away_from_zero() {
        let g = pic((101, 51), vec![Step::Crop([50, 1, 25, 1, 0, 1, 0, 1])]).geometry();
        assert_eq!((g.x, g.y, g.w, g.h), (26, 13, 50, 25));
    }

    #[test]
    fn taps_keep_their_weight_and_reach() {
        for (len, new) in [(4032, 1200), (512, 511), (7, 3), (3, 7), (1, 1), (10, 10)] {
            let t = Taps::new(len, new);
            for o in 0..new {
                let (left, ws) = t.get(o);
                assert!(left + ws.len() <= len);
                assert!((ws.iter().sum::<f32>() - 1.).abs() < 1e-4, "{len}->{new} at {o}");
            }
        }
    }

    #[test]
    fn down_emits_every_row_and_keeps_flat_colour_flat() {
        for (h, nh) in [(100, 37), (37, 37), (5, 12)] {
            let mut d = Down::new(h, (3, nh));
            for y in 0..h {
                d.push(y, &[10., 20., 30., 255.].repeat(3));
            }
            assert!(d.done());
            assert!(d.out.chunks_exact(4).all(|p| p == [10, 20, 30, 255]), "{h}->{nh}");
        }
    }

    #[test]
    fn matrix_matches_bt601_full_and_limited() {
        let m = Matrix::new(None, (None, None), 8);
        assert_eq!(m.rgb(128., Some((128., 128.))).map(|v| v.round()), [128., 128., 128.]);
        let [r, g, b] = m.rgb(76., Some((85., 255.)));
        assert!((r - 254.).abs() < 2. && g.abs() < 2. && b.abs() < 2., "{r} {g} {b}");
        let m = Matrix::new(Some(Nclx { matrix: 1, full: false }), (None, None), 10);
        assert_eq!(m.rgb(64., Some((512., 512.))).map(|v| v.round()), [0., 0., 0.]);
        assert_eq!(m.rgb(940., Some((512., 512.))).map(|v| v.round()), [255., 255., 255.]);
    }

    #[test]
    fn orient_turns_and_mirrors_into_bgra() {
        // 2×1: red then green.
        let rgba = [255, 0, 0, 255, 0, 255, 0, 255];
        let g = Geometry { x: 0, y: 0, w: 2, h: 1, rot: 90, flip: false };
        let out = orient(&rgba, 2, 1, &g, false);
        assert_eq!((out.width(), out.height()), (1, 2));
        // Turned clockwise, the left pixel is on top; BGRA, so red is in the third byte.
        assert_eq!(out.as_raw()[..8], [0, 0, 255, 255, 0, 255, 0, 255]);
        let g = Geometry { rot: 0, flip: true, ..g };
        assert_eq!(orient(&rgba, 2, 1, &g, false).as_raw()[..4], [0, 255, 0, 255]);
    }

    /// Damaged and cut files to noise: none may panic.
    #[test]
    fn parsing_never_panics() {
        let mut seed = 0x2545F4914F6CDD1Du64;
        let mut next = || {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed
        };
        let head = {
            let mut b = b"heic".to_vec();
            b.extend_from_slice(&[0; 4]);
            b.extend_from_slice(b"mif1");
            bx(b"ftyp", &b)
        };
        for _ in 0..20_000 {
            let mut f = head.clone();
            let n = (next() % 600) as usize;
            let mut body: Vec<u8> = (0..n).map(|_| next() as u8).collect();
            // Plausible box names, so the parser goes deep.
            for k in [b"meta", b"iinf", b"iloc", b"iprp", b"ipco", b"ipma", b"iref", b"pitm", b"infe", b"ispe", b"grid", b"dimg"] {
                if next() % 3 == 0 && body.len() >= 8 {
                    let at = (next() as usize) % (body.len() - 7);
                    body[at + 4..at + 8].copy_from_slice(k);
                }
            }
            f.extend_from_slice(&bx(b"meta", &body));
            if let Some(h) = Heif::parse(&f)
                && let Some(p) = h.picture(h.primary)
            {
                let _ = p.geometry();
                let _ = h.alpha_of(h.primary);
                let _ = h.thumbnails().count();
            }
        }
    }

    #[cfg(feature = "video")]
    fn fixture(name: &str) -> Vec<u8> {
        std::fs::read(format!("{}/reference/media/{name}", env!("CARGO_MANIFEST_DIR"))).unwrap()
    }

    /// A 2×2 grid of red, green / blue, white cropped to 60×56, turned a quarter anticlockwise
    /// and mirrored top to bottom, with alpha clear over its right third: libheif shows the
    /// grid transposed, clear below row 40.
    #[cfg(feature = "video")]
    #[test]
    fn the_grid_fixture_shows_as_libheif_shows_it() {
        let (img, w, h) = decode(&fixture("grid.heic"), (1000, 1000)).unwrap();
        assert_eq!((w, h, img.width(), img.height()), (56, 60, 56, 60));
        let at = |x: u32, y: u32| img.get_pixel(x, y).0;
        let near = |a: [u8; 4], b: [u8; 4]| a.iter().zip(b).all(|(a, b)| a.abs_diff(b) <= 6);
        for (x, y, bgra) in [
            (0, 0, [0, 0, 255, 255]),
            (55, 0, [255, 0, 0, 255]),
            (45, 20, [255, 0, 0, 255]),
            (45, 35, [255, 255, 255, 255]),
            (10, 38, [0, 255, 0, 255]),
        ] {
            assert!(near(at(x, y), bgra), "({x}, {y}) is {:?}", at(x, y));
        }
        assert_eq!(at(10, 42)[3], 0);
        assert_eq!(at(55, 59)[3], 0);
        let (small, _, _) = decode(&fixture("grid.heic"), (28, 28)).unwrap();
        assert_eq!((small.width(), small.height()), (26, 28));
        assert!(near(small.get_pixel(2, 2).0, [0, 0, 255, 255]));
    }

    #[cfg(feature = "video")]
    #[test]
    fn a_sequence_plays_every_frame() {
        let bytes = fixture("sequence.heics");
        assert!(sniff(&bytes) && has_track(&bytes));
        let (img, w, h) = crate::media::decode(&bytes, "heic", (640, 640)).unwrap();
        assert_eq!((w, h, img.frame_count()), (64, 48, 6));
        assert!((0..6).all(|i| img.delay(i).numer_denom_ms() == (100, 1)));
    }

    /// Damaged fixtures through the whole decode: none may panic.
    #[cfg(feature = "video")]
    #[test]
    fn decoding_damaged_files_never_panics() {
        let mut seed = 0x9E3779B97F4A7C15u64;
        let mut next = || {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed
        };
        let clean = fixture("grid.heic");
        for round in 0..1500 {
            let mut f = clean.clone();
            for _ in 0..1 + next() % 4 {
                let at = (next() as usize) % f.len();
                f[at] = next() as u8;
            }
            if round % 7 == 0 {
                f.truncate((next() as usize) % f.len());
            }
            let _ = decode(&f, (40, 40));
        }
    }

    /// Against reference renders: `HEIF_CORPUS` a folder of files, `HEIF_REF` a PNG per file named
    /// by its path under the corpus with `/` as `_`, and `-N` when the file holds several.
    #[cfg(feature = "video")]
    #[test]
    fn matches_reference_renders() {
        let (Ok(corpus), Ok(refs)) = (std::env::var("HEIF_CORPUS"), std::env::var("HEIF_REF")) else { return };
        let corpus = std::path::PathBuf::from(corpus);
        let mut files = Vec::new();
        let mut dirs = vec![corpus.clone()];
        while let Some(d) = dirs.pop() {
            for e in std::fs::read_dir(d).unwrap().flatten() {
                let p = e.path();
                if p.is_dir() { dirs.push(p) } else { files.push(p) }
            }
        }
        files.sort();
        let mut worst = 0f64;
        for f in files {
            let rel = f.strip_prefix(&corpus).unwrap().to_string_lossy().replace('/', "_");
            let bytes = std::fs::read(&f).unwrap();
            if has_track(&bytes) {
                let frames = crate::media::decode(&bytes, "heic", (512, 512)).map(|(i, _, _)| i.frame_count());
                println!("{rel}: a track, plays {frames:?} frames");
            }
            let t = std::time::Instant::now();
            let ours = decode(&bytes, (1024, 1024));
            let ms = t.elapsed().as_millis();
            let refs: Vec<image::RgbaImage> = std::iter::once(format!("{refs}/corpus_{rel}.png"))
                .chain((0..8).map(|i| format!("{refs}/corpus_{rel}-{i}.png")))
                .filter_map(|p| image::open(p).ok().map(|i| i.into_rgba8()))
                .collect();
            let Some((img, w, h)) = ours else {
                println!("{rel}: none ({} refs)", refs.len());
                continue;
            };
            if let Ok(dir) = std::env::var("HEIF_DUMP") {
                let mut rgba = img.clone();
                rgba.chunks_exact_mut(4).for_each(|p| p.swap(0, 2));
                rgba.save(format!("{dir}/{rel}.png")).unwrap();
            }
            let best = refs
                .iter()
                .filter(|r| (r.width(), r.height()) == (w, h))
                .map(|r| {
                    let r = image::imageops::resize(r, img.width(), img.height(), image::imageops::FilterType::CatmullRom);
                    let mut err = 0f64;
                    for (a, b) in img.as_raw().chunks_exact(4).zip(r.as_raw().chunks_exact(4)) {
                        let ours = [a[2], a[1], a[0], a[3]];
                        let k = if b[3] == 0 { 0 } else { 3 };
                        for c in (0..k).chain(3..4) {
                            err += (f64::from(ours[c]) - f64::from(b[c])).abs();
                        }
                    }
                    err / (img.width() * img.height()) as f64 / 4.
                })
                .fold(f64::INFINITY, f64::min);
            println!("{rel}: {w}x{h} shown {}x{} in {ms} ms, mean error {best:.2} ({} refs)", img.width(), img.height(), refs.len());
            if best.is_finite() {
                worst = worst.max(best);
            }
        }
        println!("worst {worst:.2}");
    }
}
