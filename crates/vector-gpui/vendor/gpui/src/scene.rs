// todo("windows"): remove
#![cfg_attr(windows, allow(dead_code))]

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::{
    AtlasTextureId, AtlasTile, Background, Bounds, ContentMask, Corners, Edges, Hsla, Pixels,
    Point, Radians, ScaledPixels, Size, bounds_tree::BoundsTree, point, size,
};
use std::{
    fmt::Debug,
    iter::Peekable,
    ops::{Add, Range, Sub},
    slice,
};

#[allow(non_camel_case_types, unused)]
#[expect(missing_docs)]
pub type PathVertex_ScaledPixels = PathVertex<ScaledPixels>;

#[expect(missing_docs)]
pub type DrawOrder = u32;

/// A boolean stored as a `u32` so that GPU-facing structs contain no
/// compiler-inserted padding bytes, which would be undefined behavior to
/// reinterpret as `&[u8]` when writing instance buffers. Guaranteed to be
/// `0` or `1` by construction; shaders read it as a `u32`/`uint`.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
#[repr(transparent)]
pub struct PaddedBool32(u32);

impl From<bool> for PaddedBool32 {
    fn from(value: bool) -> Self {
        PaddedBool32(value as u32)
    }
}

#[derive(Default)]
#[expect(missing_docs)]
pub struct Scene {
    pub(crate) paint_operations: Vec<PaintOperation>,
    primitive_bounds: BoundsTree<ScaledPixels>,
    layer_stack: Vec<DrawOrder>,
    pub shadows: Vec<Shadow>,
    pub quads: Vec<Quad>,
    pub paths: Vec<Path<ScaledPixels>>,
    pub underlines: Vec<Underline>,
    pub monochrome_sprites: Vec<MonochromeSprite>,
    pub subpixel_sprites: Vec<SubpixelSprite>,
    pub polychrome_sprites: Vec<PolychromeSprite>,
    pub surfaces: Vec<PaintSurface>,
    pub backdrops: Vec<Backdrop>,
    /// Sort keys with each primitive's index, kept between frames so sorting allocates nothing.
    sort_keys: Vec<(u64, u32)>,
    /// While a zoomed subtree paints: `p -> a + p * k` on every primitive's geometry, in device
    /// pixels. Content masks are mapped where they are pushed, so outer clips stay put.
    pub(crate) zoom: Option<(Point<ScaledPixels>, f32)>,
    /// Clips that hold over a keyed subtree, which records its primitives clipped by its own
    /// masks alone, so it can be replayed elsewhere: (outer mask, scale factor).
    outer: Vec<(ContentMask<Pixels>, f32)>,
}

fn overdraw_report() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("VECTOR_OVERDRAW").is_some())
}

/// Sorts `items` by `key`, keeping insertion order among equal keys (a layer's primitives share
/// one order and must draw as painted), through `keys` rather than a fresh merge buffer.
fn sort_stable<T>(items: &mut [T], keys: &mut Vec<(u64, u32)>, key: impl Fn(&T) -> u64) {
    keys.clear();
    keys.extend(items.iter().enumerate().map(|(i, item)| (key(item), i as u32)));
    if keys.is_sorted() {
        return;
    }
    keys.sort_unstable();
    // Position `j` takes the item from `keys[j].1`: follow each cycle once, marking it done.
    const DONE: u32 = u32::MAX;
    for start in 0..items.len() {
        let mut at = start;
        loop {
            let from = keys[at].1;
            if from == DONE {
                break;
            }
            keys[at].1 = DONE;
            if from as usize == start {
                break;
            }
            items.swap(at, from as usize);
            at = from as usize;
        }
    }
}

#[expect(missing_docs)]
impl Scene {
    pub fn clear(&mut self) {
        self.paint_operations.clear();
        self.primitive_bounds.clear();
        self.layer_stack.clear();
        self.paths.clear();
        self.shadows.clear();
        self.quads.clear();
        self.underlines.clear();
        self.monochrome_sprites.clear();
        self.subpixel_sprites.clear();
        self.polychrome_sprites.clear();
        self.surfaces.clear();
        self.backdrops.clear();
        self.outer.clear();
    }

    pub fn len(&self) -> usize {
        self.paint_operations.len()
    }

    pub fn push_layer(&mut self, bounds: Bounds<ScaledPixels>) {
        let order = self.primitive_bounds.insert(bounds);
        self.layer_stack.push(order);
        self.paint_operations
            .push(PaintOperation::StartLayer(bounds));
    }

    pub fn pop_layer(&mut self) {
        self.layer_stack.pop();
        self.paint_operations.push(PaintOperation::EndLayer);
    }

    pub(crate) fn push_outer(&mut self, mask: ContentMask<Pixels>, scale: f32) {
        self.paint_operations.push(PaintOperation::PushOuter(mask.clone(), scale));
        let mask = match self.outer.last() {
            Some((o, _)) => mask.intersect(o),
            None => mask,
        };
        self.outer.push((mask, scale));
    }

    pub(crate) fn pop_outer(&mut self) {
        self.outer.pop();
        self.paint_operations.push(PaintOperation::PopOuter);
    }

    /// Replays `range` of `prev` moved by `by` device pixels: geometry and the subtree's own
    /// masks move, while the clips around it are this frame's.
    pub(crate) fn replay_shifted(&mut self, range: Range<usize>, prev: &Scene, by: Point<Pixels>, scale: f32) {
        let d = point(ScaledPixels(by.x.0 * scale), ScaledPixels(by.y.0 * scale));
        let shift = |b: Bounds<ScaledPixels>| Bounds { origin: point(b.origin.x + d.x, b.origin.y + d.y), size: b.size };
        for operation in &prev.paint_operations[range] {
            match operation {
                PaintOperation::Primitive(primitive) => {
                    let mut primitive = primitive.clone();
                    primitive.zoom(d, 1.);
                    primitive.shift_mask(d);
                    self.insert_primitive(primitive);
                }
                PaintOperation::StartLayer(bounds) => self.push_layer(shift(*bounds)),
                PaintOperation::EndLayer => self.pop_layer(),
                PaintOperation::PushOuter(mask, scale) => {
                    let mut mask = mask.clone();
                    mask.shift(by);
                    self.push_outer(mask, *scale)
                }
                PaintOperation::PopOuter => self.pop_outer(),
            }
        }
    }

    pub fn insert_primitive(&mut self, primitive: impl Into<Primitive>) {
        let mut primitive = primitive.into();
        if let Some((a, k)) = self.zoom {
            primitive.zoom(a, k);
        }
        if let Some((outer, scale)) = self.outer.last() {
            // Recorded clipped by its own masks only, so a shifted replay can clip it afresh.
            let mut clipped = primitive.clone();
            let mask = clipped.content_mask().unscale(*scale).intersect(outer).scale(*scale);
            *clipped.content_mask_mut() = mask;
            self.push_primitive(clipped);
            self.paint_operations.push(PaintOperation::Primitive(primitive));
            return;
        }
        if self.push_primitive(primitive.clone()) {
            self.paint_operations.push(PaintOperation::Primitive(primitive));
        }
    }

    /// Adds `primitive` to its kind's list; false when its mask hides all of it.
    fn push_primitive(&mut self, mut primitive: Primitive) -> bool {
        let clipped_bounds = primitive
            .bounds()
            .intersect(&primitive.content_mask().bounds);

        if clipped_bounds.is_empty() {
            return false;
        }

        let order = self
            .layer_stack
            .last()
            .copied()
            .unwrap_or_else(|| self.primitive_bounds.insert(clipped_bounds));
        match &mut primitive {
            Primitive::Shadow(shadow) => {
                shadow.order = order;
                self.shadows.push(*shadow);
            }
            Primitive::Quad(quad) => {
                quad.order = order;
                self.quads.push(*quad);
            }
            Primitive::Path(path) => {
                path.order = order;
                path.id = PathId(self.paths.len());
                self.paths.push(path.clone());
            }
            Primitive::Underline(underline) => {
                underline.order = order;
                self.underlines.push(*underline);
            }
            Primitive::MonochromeSprite(sprite) => {
                sprite.order = order;
                self.monochrome_sprites.push(*sprite);
            }
            Primitive::SubpixelSprite(sprite) => {
                sprite.order = order;
                self.subpixel_sprites.push(*sprite);
            }
            Primitive::PolychromeSprite(sprite) => {
                sprite.order = order;
                self.polychrome_sprites.push(*sprite);
            }
            Primitive::Surface(surface) => {
                surface.order = order;
                self.surfaces.push(surface.clone());
            }
            Primitive::Backdrop(backdrop) => {
                backdrop.order = order;
                self.backdrops.push(backdrop.clone());
            }
        }
        true
    }

    pub fn replay(&mut self, range: Range<usize>, prev_scene: &Scene) {
        let _timed = crate::draw_stats::Stage::start(crate::draw_stats::REPLAY, range.len());
        for operation in &prev_scene.paint_operations[range] {
            match operation {
                PaintOperation::Primitive(primitive) => self.insert_primitive(primitive.clone()),
                PaintOperation::StartLayer(bounds) => self.push_layer(*bounds),
                PaintOperation::EndLayer => self.pop_layer(),
                PaintOperation::PushOuter(mask, scale) => self.push_outer(mask.clone(), *scale),
                PaintOperation::PopOuter => self.pop_outer(),
            }
        }
    }

    pub fn finish(&mut self) {
        let _timed = crate::draw_stats::Stage::start(crate::draw_stats::FINISH, self.paint_operations.len());
        let keys = &mut self.sort_keys;
        let order = |order: DrawOrder| (order as u64) << 32;
        sort_stable(&mut self.shadows, keys, |shadow| order(shadow.order));
        sort_stable(&mut self.quads, keys, |quad| order(quad.order));
        sort_stable(&mut self.paths, keys, |path| order(path.order));
        sort_stable(&mut self.underlines, keys, |underline| order(underline.order));
        sort_stable(&mut self.monochrome_sprites, keys, |sprite| {
            order(sprite.order) | sprite.tile.texture_id.index as u64
        });
        sort_stable(&mut self.subpixel_sprites, keys, |sprite| {
            order(sprite.order) | sprite.tile.texture_id.index as u64
        });
        sort_stable(&mut self.polychrome_sprites, keys, |sprite| {
            order(sprite.order) | sprite.tile.texture_id.index as u64
        });
        sort_stable(&mut self.surfaces, keys, |surface| order(surface.order));
        sort_stable(&mut self.backdrops, keys, |backdrop| order(backdrop.order));
        self.cull_covered();
        if overdraw_report() {
            self.report_overdraw();
        }
    }

    /// `VECTOR_OVERDRAW=1`: each frame's blended area by kind, and its largest quads, to stderr.
    fn report_overdraw(&self) {
        let area = |b: &Bounds<ScaledPixels>| (b.size.width.0.max(0.) * b.size.height.0.max(0.)) as f64;
        let quads: Vec<(f64, &Quad)> = self.quads.iter().map(|q| (area(&q.bounds.intersect(&q.content_mask.bounds)), q)).collect();
        let shadows: f64 = self.shadows.iter().map(|s| area(&s.bounds.dilate(ScaledPixels(3.0 * s.blur_radius.0)).intersect(&s.content_mask.bounds))).sum();
        let mono: f64 = self.monochrome_sprites.iter().map(|g| area(&g.bounds.intersect(&g.content_mask.bounds))).sum();
        let poly: f64 = self.polychrome_sprites.iter().map(|g| area(&g.bounds.intersect(&g.content_mask.bounds))).sum();
        let paths: f64 = self.paths.iter().map(|p| area(&p.clipped_bounds())).sum();
        let total_q: f64 = quads.iter().map(|q| q.0).sum();
        eprintln!(
            "overdraw: quads {} {:.0}k px, shadows {:.0}k, glyphs {:.0}k, images {:.0}k, paths {:.0}k",
            quads.len(), total_q / 1e3, shadows / 1e3, mono / 1e3, poly / 1e3, paths / 1e3
        );
        let mut big = quads;
        big.sort_by(|a, b| b.0.total_cmp(&a.0));
        for (a, q) in big.iter().take(12) {
            let b = q.bounds.intersect(&q.content_mask.bounds);
            eprintln!(
                "  {:.0}k px order {} at {:.2},{:.2} {:.2}x{:.2} bg {:?} opaque {} r {:?} mr {:?} fade {:?}",
                a / 1e3, q.order, b.origin.x.0, b.origin.y.0, b.size.width.0, b.size.height.0,
                q.background, q.opaque_rect().is_some(), q.corner_radii, q.content_mask.corner_radii, q.content_mask.fade
            );
        }
    }

    /// Drops what a later opaque rectangle hides whole. Everything draws blended, so a tiled
    /// GPU can't discard hidden pixels itself, and each stacked full-window background costs a
    /// window of fill.
    fn cull_covered(&mut self) {
        const MAX_COVERS: usize = 6;
        let mut covers = smallvec::SmallVec::<[(DrawOrder, Bounds<ScaledPixels>); MAX_COVERS]>::new();
        for quad in &self.quads {
            let Some(rect) = quad.opaque_rect() else { continue };
            let area = rect.size.width.0 * rect.size.height.0;
            // Small covers hide little and cost a check per primitive.
            if area < 128.0 * 128.0 {
                continue;
            }
            let smallest = covers
                .iter()
                .enumerate()
                .min_by(|a, b| area_of(&a.1.1).total_cmp(&area_of(&b.1.1)))
                .map(|(i, c)| (i, area_of(&c.1)));
            match smallest {
                _ if covers.len() < MAX_COVERS => covers.push((quad.order, rect)),
                Some((i, least)) if least < area => covers[i] = (quad.order, rect),
                _ => {}
            }
        }
        if covers.is_empty() {
            return;
        }
        let hidden = |order: DrawOrder, bounds: Bounds<ScaledPixels>| {
            covers.iter().any(|(above, cover)| order < *above && encloses(cover, &bounds))
        };
        self.shadows.retain(|s| {
            let reach = s.bounds.dilate(ScaledPixels(3.0 * s.blur_radius.0)).union(&s.element_bounds);
            !hidden(s.order, reach.intersect(&s.content_mask.bounds))
        });
        let mut pieces = smallvec::SmallVec::<[(usize, smallvec::SmallVec<[Quad; 3]>); 2]>::new();
        for (i, q) in self.quads.iter_mut().enumerate() {
            let rest = q.trim_under(&covers);
            if !rest.is_empty() {
                pieces.push((i + 1, rest));
            }
        }
        // Next to their quad, from the back: the order sort still holds.
        for (at, rest) in pieces.into_iter().rev() {
            self.quads.splice(at..at, rest);
        }
        self.quads.retain(|q| !hidden(q.order, q.bounds.intersect(&q.content_mask.bounds)));
        // Paths and surfaces may ignore their mask: only their own bounds are safe.
        self.paths.retain(|p| !hidden(p.order, p.bounds));
        self.monochrome_sprites.retain(|g| {
            // A transformed glyph can leave its bounds; its mask still clips it.
            let extent = if g.transformation == TransformationMatrix::unit() {
                g.bounds.intersect(&g.content_mask.bounds)
            } else {
                g.content_mask.bounds
            };
            !hidden(g.order, extent)
        });
        self.subpixel_sprites.retain(|g| {
            let extent = if g.transformation == TransformationMatrix::unit() {
                g.bounds.intersect(&g.content_mask.bounds)
            } else {
                g.content_mask.bounds
            };
            !hidden(g.order, extent)
        });
        self.polychrome_sprites.retain(|i| !hidden(i.order, i.bounds.intersect(&i.content_mask.bounds)));
        self.surfaces.retain(|v| !hidden(v.order, v.bounds));
        self.backdrops.retain(|b| !hidden(b.order, b.bounds.intersect(&b.content_mask.bounds)));
    }

    /// Whether no two path batches overlap, so a renderer can rasterize every path into one
    /// intermediate before compositing: a tiled GPU then never breaks its main pass for paths.
    pub fn path_batches_disjoint(&self) -> bool {
        let mut seen = smallvec::SmallVec::<[Bounds<ScaledPixels>; 16]>::new();
        for batch in self.batches() {
            let PrimitiveBatch::Paths(range) = batch else { continue };
            let mut paths = self.paths[range].iter();
            let Some(first) = paths.next() else { continue };
            let bounds = paths
                .fold(first.clipped_bounds(), |bounds, path| bounds.union(&path.clipped_bounds()))
                .dilate(ScaledPixels(1.));
            if seen.iter().any(|other| other.intersects(&bounds)) {
                return false;
            }
            seen.push(bounds);
        }
        true
    }

    #[cfg_attr(
        all(
            any(target_os = "linux", target_os = "freebsd"),
            not(any(feature = "x11", feature = "wayland"))
        ),
        allow(dead_code)
    )]
    pub fn batches(&self) -> impl Iterator<Item = PrimitiveBatch> + '_ {
        BatchIterator {
            shadows_start: 0,
            shadows_iter: self.shadows.iter().peekable(),
            quads_start: 0,
            quads_iter: self.quads.iter().peekable(),
            paths_start: 0,
            paths_iter: self.paths.iter().peekable(),
            underlines_start: 0,
            underlines_iter: self.underlines.iter().peekable(),
            monochrome_sprites_start: 0,
            monochrome_sprites_iter: self.monochrome_sprites.iter().peekable(),
            subpixel_sprites_start: 0,
            subpixel_sprites_iter: self.subpixel_sprites.iter().peekable(),
            polychrome_sprites_start: 0,
            polychrome_sprites_iter: self.polychrome_sprites.iter().peekable(),
            surfaces_start: 0,
            surfaces_iter: self.surfaces.iter().peekable(),
            backdrops_start: 0,
            backdrops_iter: self.backdrops.iter().peekable(),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Default)]
#[cfg_attr(
    all(
        any(target_os = "linux", target_os = "freebsd"),
        not(any(feature = "x11", feature = "wayland"))
    ),
    allow(dead_code)
)]
pub(crate) enum PrimitiveKind {
    Shadow,
    #[default]
    Quad,
    Path,
    Underline,
    MonochromeSprite,
    SubpixelSprite,
    PolychromeSprite,
    Surface,
    Backdrop,
}

pub(crate) enum PaintOperation {
    Primitive(Primitive),
    StartLayer(Bounds<ScaledPixels>),
    EndLayer,
    /// The clip a keyed subtree's primitives take on top of their own masks.
    PushOuter(ContentMask<Pixels>, f32),
    PopOuter,
}

#[derive(Clone)]
#[expect(missing_docs)]
pub enum Primitive {
    Shadow(Shadow),
    Quad(Quad),
    Path(Path<ScaledPixels>),
    Underline(Underline),
    MonochromeSprite(MonochromeSprite),
    SubpixelSprite(SubpixelSprite),
    PolychromeSprite(PolychromeSprite),
    Surface(PaintSurface),
    Backdrop(Backdrop),
}

impl Primitive {
    /// Map the geometry through `p -> a + p * k`.
    fn zoom(&mut self, a: Point<ScaledPixels>, k: f32) {
        let at = |p: Point<ScaledPixels>| point(a.x + p.x * k, a.y + p.y * k);
        let b = |r: Bounds<ScaledPixels>| Bounds { origin: at(r.origin), size: size(r.size.width * k, r.size.height * k) };
        let c = |r: Corners<ScaledPixels>| Corners {
            top_left: r.top_left * k,
            top_right: r.top_right * k,
            bottom_right: r.bottom_right * k,
            bottom_left: r.bottom_left * k,
        };
        match self {
            Primitive::Shadow(x) => {
                x.bounds = b(x.bounds);
                x.blur_radius = x.blur_radius * k;
                x.corner_radii = c(x.corner_radii);
                x.element_bounds = b(x.element_bounds);
                x.element_corner_radii = c(x.element_corner_radii);
            }
            Primitive::Quad(x) => {
                x.bounds = b(x.bounds);
                x.corner_radii = c(x.corner_radii);
                x.border_widths = Edges {
                    top: x.border_widths.top * k,
                    right: x.border_widths.right * k,
                    bottom: x.border_widths.bottom * k,
                    left: x.border_widths.left * k,
                };
            }
            Primitive::Path(x) => {
                x.bounds = b(x.bounds);
                for v in &mut x.vertices {
                    v.xy_position = at(v.xy_position);
                }
            }
            Primitive::Underline(x) => {
                x.bounds = b(x.bounds);
                x.thickness = x.thickness * k;
            }
            Primitive::MonochromeSprite(x) => x.bounds = b(x.bounds),
            Primitive::SubpixelSprite(x) => x.bounds = b(x.bounds),
            Primitive::PolychromeSprite(x) => {
                x.bounds = b(x.bounds);
                x.corner_radii = c(x.corner_radii);
            }
            Primitive::Surface(x) => x.bounds = b(x.bounds),
            Primitive::Backdrop(x) => {
                x.bounds = b(x.bounds);
                x.corner_radii = c(x.corner_radii);
                x.blur_radius = x.blur_radius * k;
            }
        }
    }
}

#[expect(missing_docs)]
impl Primitive {
    pub fn bounds(&self) -> &Bounds<ScaledPixels> {
        match self {
            Primitive::Shadow(shadow) => &shadow.bounds,
            Primitive::Quad(quad) => &quad.bounds,
            Primitive::Path(path) => &path.bounds,
            Primitive::Underline(underline) => &underline.bounds,
            Primitive::MonochromeSprite(sprite) => &sprite.bounds,
            Primitive::SubpixelSprite(sprite) => &sprite.bounds,
            Primitive::PolychromeSprite(sprite) => &sprite.bounds,
            Primitive::Surface(surface) => &surface.bounds,
            Primitive::Backdrop(backdrop) => &backdrop.bounds,
        }
    }

    fn content_mask_mut(&mut self) -> &mut ContentMask<ScaledPixels> {
        match self {
            Primitive::Shadow(x) => &mut x.content_mask,
            Primitive::Quad(x) => &mut x.content_mask,
            Primitive::Path(x) => &mut x.content_mask,
            Primitive::Underline(x) => &mut x.content_mask,
            Primitive::MonochromeSprite(x) => &mut x.content_mask,
            Primitive::SubpixelSprite(x) => &mut x.content_mask,
            Primitive::PolychromeSprite(x) => &mut x.content_mask,
            Primitive::Surface(x) => &mut x.content_mask,
            Primitive::Backdrop(x) => &mut x.content_mask,
        }
    }

    fn shift_mask(&mut self, d: Point<ScaledPixels>) {
        self.content_mask_mut().shift(d);
    }

    pub fn content_mask(&self) -> &ContentMask<ScaledPixels> {
        match self {
            Primitive::Shadow(shadow) => &shadow.content_mask,
            Primitive::Quad(quad) => &quad.content_mask,
            Primitive::Path(path) => &path.content_mask,
            Primitive::Underline(underline) => &underline.content_mask,
            Primitive::MonochromeSprite(sprite) => &sprite.content_mask,
            Primitive::SubpixelSprite(sprite) => &sprite.content_mask,
            Primitive::PolychromeSprite(sprite) => &sprite.content_mask,
            Primitive::Surface(surface) => &surface.content_mask,
            Primitive::Backdrop(backdrop) => &backdrop.content_mask,
        }
    }
}

#[cfg_attr(
    all(
        any(target_os = "linux", target_os = "freebsd"),
        not(any(feature = "x11", feature = "wayland"))
    ),
    allow(dead_code)
)]
struct BatchIterator<'a> {
    shadows_start: usize,
    shadows_iter: Peekable<slice::Iter<'a, Shadow>>,
    quads_start: usize,
    quads_iter: Peekable<slice::Iter<'a, Quad>>,
    paths_start: usize,
    paths_iter: Peekable<slice::Iter<'a, Path<ScaledPixels>>>,
    underlines_start: usize,
    underlines_iter: Peekable<slice::Iter<'a, Underline>>,
    monochrome_sprites_start: usize,
    monochrome_sprites_iter: Peekable<slice::Iter<'a, MonochromeSprite>>,
    subpixel_sprites_start: usize,
    subpixel_sprites_iter: Peekable<slice::Iter<'a, SubpixelSprite>>,
    polychrome_sprites_start: usize,
    polychrome_sprites_iter: Peekable<slice::Iter<'a, PolychromeSprite>>,
    surfaces_start: usize,
    surfaces_iter: Peekable<slice::Iter<'a, PaintSurface>>,
    backdrops_start: usize,
    backdrops_iter: Peekable<slice::Iter<'a, Backdrop>>,
}

impl<'a> Iterator for BatchIterator<'a> {
    type Item = PrimitiveBatch;

    fn next(&mut self) -> Option<Self::Item> {
        let mut orders_and_kinds = [
            (
                self.shadows_iter.peek().map(|s| s.order),
                PrimitiveKind::Shadow,
            ),
            (self.quads_iter.peek().map(|q| q.order), PrimitiveKind::Quad),
            (self.paths_iter.peek().map(|q| q.order), PrimitiveKind::Path),
            (
                self.underlines_iter.peek().map(|u| u.order),
                PrimitiveKind::Underline,
            ),
            (
                self.monochrome_sprites_iter.peek().map(|s| s.order),
                PrimitiveKind::MonochromeSprite,
            ),
            (
                self.subpixel_sprites_iter.peek().map(|s| s.order),
                PrimitiveKind::SubpixelSprite,
            ),
            (
                self.polychrome_sprites_iter.peek().map(|s| s.order),
                PrimitiveKind::PolychromeSprite,
            ),
            (
                self.surfaces_iter.peek().map(|s| s.order),
                PrimitiveKind::Surface,
            ),
            (
                self.backdrops_iter.peek().map(|b| b.order),
                PrimitiveKind::Backdrop,
            ),
        ];
        orders_and_kinds.sort_by_key(|(order, kind)| (order.unwrap_or(u32::MAX), *kind));

        let first = orders_and_kinds[0];
        let second = orders_and_kinds[1];
        let (batch_kind, max_order_and_kind) = if first.0.is_some() {
            (first.1, (second.0.unwrap_or(u32::MAX), second.1))
        } else {
            return None;
        };

        match batch_kind {
            PrimitiveKind::Shadow => {
                let shadows_start = self.shadows_start;
                let mut shadows_end = shadows_start + 1;
                self.shadows_iter.next();
                while self
                    .shadows_iter
                    .next_if(|shadow| (shadow.order, batch_kind) < max_order_and_kind)
                    .is_some()
                {
                    shadows_end += 1;
                }
                self.shadows_start = shadows_end;
                Some(PrimitiveBatch::Shadows(shadows_start..shadows_end))
            }
            PrimitiveKind::Quad => {
                let quads_start = self.quads_start;
                let mut quads_end = quads_start + 1;
                self.quads_iter.next();
                while self
                    .quads_iter
                    .next_if(|quad| (quad.order, batch_kind) < max_order_and_kind)
                    .is_some()
                {
                    quads_end += 1;
                }
                self.quads_start = quads_end;
                Some(PrimitiveBatch::Quads(quads_start..quads_end))
            }
            PrimitiveKind::Path => {
                let paths_start = self.paths_start;
                let mut paths_end = paths_start + 1;
                self.paths_iter.next();
                while self
                    .paths_iter
                    .next_if(|path| (path.order, batch_kind) < max_order_and_kind)
                    .is_some()
                {
                    paths_end += 1;
                }
                self.paths_start = paths_end;
                Some(PrimitiveBatch::Paths(paths_start..paths_end))
            }
            PrimitiveKind::Underline => {
                let underlines_start = self.underlines_start;
                let mut underlines_end = underlines_start + 1;
                self.underlines_iter.next();
                while self
                    .underlines_iter
                    .next_if(|underline| (underline.order, batch_kind) < max_order_and_kind)
                    .is_some()
                {
                    underlines_end += 1;
                }
                self.underlines_start = underlines_end;
                Some(PrimitiveBatch::Underlines(underlines_start..underlines_end))
            }
            PrimitiveKind::MonochromeSprite => {
                let texture_id = self.monochrome_sprites_iter.peek().unwrap().tile.texture_id;
                let sprites_start = self.monochrome_sprites_start;
                let mut sprites_end = sprites_start + 1;
                self.monochrome_sprites_iter.next();
                while self
                    .monochrome_sprites_iter
                    .next_if(|sprite| {
                        (sprite.order, batch_kind) < max_order_and_kind
                            && sprite.tile.texture_id == texture_id
                    })
                    .is_some()
                {
                    sprites_end += 1;
                }
                self.monochrome_sprites_start = sprites_end;
                Some(PrimitiveBatch::MonochromeSprites {
                    texture_id,
                    range: sprites_start..sprites_end,
                })
            }
            PrimitiveKind::SubpixelSprite => {
                let texture_id = self.subpixel_sprites_iter.peek().unwrap().tile.texture_id;
                let sprites_start = self.subpixel_sprites_start;
                let mut sprites_end = sprites_start + 1;
                self.subpixel_sprites_iter.next();
                while self
                    .subpixel_sprites_iter
                    .next_if(|sprite| {
                        (sprite.order, batch_kind) < max_order_and_kind
                            && sprite.tile.texture_id == texture_id
                    })
                    .is_some()
                {
                    sprites_end += 1;
                }
                self.subpixel_sprites_start = sprites_end;
                Some(PrimitiveBatch::SubpixelSprites {
                    texture_id,
                    range: sprites_start..sprites_end,
                })
            }
            PrimitiveKind::PolychromeSprite => {
                let texture_id = self.polychrome_sprites_iter.peek().unwrap().tile.texture_id;
                let sprites_start = self.polychrome_sprites_start;
                let mut sprites_end = sprites_start + 1;
                self.polychrome_sprites_iter.next();
                while self
                    .polychrome_sprites_iter
                    .next_if(|sprite| {
                        (sprite.order, batch_kind) < max_order_and_kind
                            && sprite.tile.texture_id == texture_id
                    })
                    .is_some()
                {
                    sprites_end += 1;
                }
                self.polychrome_sprites_start = sprites_end;
                Some(PrimitiveBatch::PolychromeSprites {
                    texture_id,
                    range: sprites_start..sprites_end,
                })
            }
            PrimitiveKind::Surface => {
                let surfaces_start = self.surfaces_start;
                let mut surfaces_end = surfaces_start + 1;
                self.surfaces_iter.next();
                while self
                    .surfaces_iter
                    .next_if(|surface| (surface.order, batch_kind) < max_order_and_kind)
                    .is_some()
                {
                    surfaces_end += 1;
                }
                self.surfaces_start = surfaces_end;
                Some(PrimitiveBatch::Surfaces(surfaces_start..surfaces_end))
            }
            PrimitiveKind::Backdrop => {
                // One at a time: each blurs what is drawn below it, which may hold the last.
                let start = self.backdrops_start;
                self.backdrops_iter.next();
                self.backdrops_start = start + 1;
                Some(PrimitiveBatch::Backdrops(start..start + 1))
            }
        }
    }
}

#[derive(Debug)]
#[cfg_attr(
    all(
        any(target_os = "linux", target_os = "freebsd"),
        not(any(feature = "x11", feature = "wayland"))
    ),
    allow(dead_code)
)]
#[allow(missing_docs)]
pub enum PrimitiveBatch {
    Shadows(Range<usize>),
    Quads(Range<usize>),
    Paths(Range<usize>),
    Underlines(Range<usize>),
    MonochromeSprites {
        texture_id: AtlasTextureId,
        range: Range<usize>,
    },
    #[cfg_attr(target_os = "macos", allow(dead_code))]
    SubpixelSprites {
        texture_id: AtlasTextureId,
        range: Range<usize>,
    },
    PolychromeSprites {
        texture_id: AtlasTextureId,
        range: Range<usize>,
    },
    Surfaces(Range<usize>),
    /// Blur what is drawn so far under each backdrop's shape, as CSS `backdrop-filter: blur()`.
    Backdrops(Range<usize>),
}

impl PrimitiveBatch {
    #[expect(missing_docs)]
    pub fn label(&self) -> String {
        match self {
            Self::Shadows(range) => format!("shadows ({})", range.len()),
            Self::Quads(range) => format!("quads ({})", range.len()),
            Self::Paths(range) => format!("paths ({})", range.len()),
            Self::Underlines(range) => format!("underlines ({})", range.len()),
            Self::MonochromeSprites { texture_id, range } => {
                format!(
                    "monochrome sprites ({}) on atlas {}",
                    range.len(),
                    texture_id.index
                )
            }
            Self::SubpixelSprites { texture_id, range } => {
                format!(
                    "subpixel sprites ({}) on atlas {}",
                    range.len(),
                    texture_id.index
                )
            }
            Self::PolychromeSprites { texture_id, range } => {
                format!(
                    "polychrome sprites ({}) on atlas {}",
                    range.len(),
                    texture_id.index
                )
            }
            Self::Surfaces(range) => format!("surfaces ({})", range.len()),
            Self::Backdrops(range) => format!("backdrops ({})", range.len()),
        }
    }
}

#[derive(Default, Debug, Copy, Clone)]
#[repr(C)]
#[expect(missing_docs)]
pub struct Quad {
    pub order: DrawOrder,
    pub border_style: BorderStyle,
    pub bounds: Bounds<ScaledPixels>,
    pub content_mask: ContentMask<ScaledPixels>,
    pub background: Background,
    pub border_color: Hsla,
    pub corner_radii: Corners<ScaledPixels>,
    pub border_widths: Edges<ScaledPixels>,
}

impl Quad {
    /// The whole device pixels this quad paints fully opaque, if it is an opaque fill under a plain
    /// rectangular mask. A border blends over an opaque fill and stays opaque.
    fn opaque_rect(&self) -> Option<Bounds<ScaledPixels>> {
        let mask = &self.content_mask;
        let square = |c: &Corners<ScaledPixels>| {
            c.top_left.0 == 0.0 && c.top_right.0 == 0.0 && c.bottom_right.0 == 0.0 && c.bottom_left.0 == 0.0
        };
        let fade = &mask.fade;
        if !self.background.is_opaque()
            || !square(&mask.corner_radii)
            || fade.top_len.0 > 0.0
            || fade.bottom_len.0 > 0.0
            || fade.left_len.0 > 0.0
            || fade.right_len.0 > 0.0
        {
            return None;
        }
        // Rounded corners leave the full-width band between them opaque.
        let r = &self.corner_radii;
        let (top, bottom) = (r.top_left.0.max(r.top_right.0), r.bottom_left.0.max(r.bottom_right.0));
        let band = Bounds {
            origin: point(self.bounds.origin.x, ScaledPixels(self.bounds.origin.y.0 + top)),
            size: size(self.bounds.size.width, ScaledPixels(self.bounds.size.height.0 - top - bottom)),
        };
        // Edge pixels a fractional bound only partly covers are antialiased, so not opaque.
        let clip = band.intersect(&mask.bounds);
        let (x0, y0) = (clip.origin.x.0.ceil(), clip.origin.y.0.ceil());
        let (x1, y1) = ((clip.origin.x.0 + clip.size.width.0).floor(), (clip.origin.y.0 + clip.size.height.0).floor());
        (x1 > x0 && y1 > y0).then(|| Bounds {
            origin: point(ScaledPixels(x0), ScaledPixels(y0)),
            size: size(ScaledPixels(x1 - x0), ScaledPixels(y1 - y0)),
        })
    }
}

impl Quad {
    /// A plain fill (solid, square, borderless) paints every pixel its bounds hold alike, so the
    /// part a later cover hides can be cut out: a backdrop left showing only round a pane shades
    /// that margin, not the window. This quad keeps one piece of what is left and returns the
    /// others, at most three, for a cover inside it.
    fn trim_under(&mut self, covers: &[(DrawOrder, Bounds<ScaledPixels>)]) -> smallvec::SmallVec<[Quad; 3]> {
        let mut rest = smallvec::SmallVec::new();
        let square = |c: &Corners<ScaledPixels>| {
            c.top_left.0 == 0.0 && c.top_right.0 == 0.0 && c.bottom_right.0 == 0.0 && c.bottom_left.0 == 0.0
        };
        let borderless = {
            let b = &self.border_widths;
            b.top.0 == 0.0 && b.right.0 == 0.0 && b.bottom.0 == 0.0 && b.left.0 == 0.0
        };
        if self.background.as_solid().is_none() || !square(&self.corner_radii) || !borderless {
            return rest;
        }
        let piece = |q: &Quad, x0: f32, y0: f32, x1: f32, y1: f32| Quad {
            bounds: Bounds {
                origin: point(ScaledPixels(x0), ScaledPixels(y0)),
                size: size(ScaledPixels(x1 - x0), ScaledPixels(y1 - y0)),
            },
            ..q.clone()
        };
        for (above, cover) in covers {
            let (x0, y0) = (self.bounds.origin.x.0, self.bounds.origin.y.0);
            let (x1, y1) = (x0 + self.bounds.size.width.0, y0 + self.bounds.size.height.0);
            let (cx0, cy0) = (cover.origin.x.0.max(x0), cover.origin.y.0.max(y0));
            let (cx1, cy1) = ((cover.origin.x.0 + cover.size.width.0).min(x1), (cover.origin.y.0 + cover.size.height.0).min(y1));
            if *above <= self.order || cx1 <= cx0 || cy1 <= cy0 {
                continue;
            }
            // What is left: the full-width bands above and below, then the sides between them.
            let mut left = smallvec::SmallVec::<[Quad; 4]>::new();
            for (px0, py0, px1, py1) in [(x0, y0, x1, cy0), (x0, cy1, x1, y1), (x0, cy0, cx0, cy1), (cx1, cy0, x1, cy1)] {
                if px1 > px0 && py1 > py0 {
                    left.push(piece(self, px0, py0, px1, py1));
                }
            }
            // Pieces of an earlier split stay as they are: one cut per quad keeps this cheap.
            let pieces_allowed = rest.is_empty() || left.len() <= 1;
            if !pieces_allowed {
                continue;
            }
            let mut left = left.into_iter();
            match left.next() {
                Some(first) => *self = first,
                // Covered whole: the cull drops it.
                None => {
                    self.bounds.size = Size::default();
                    break;
                }
            }
            rest.extend(left);
        }
        rest
    }
}

fn area_of(b: &Bounds<ScaledPixels>) -> f32 {
    b.size.width.0 * b.size.height.0
}

/// Whether `inner` lies inside `outer`; an empty `inner` draws nothing and counts as inside.
fn encloses(outer: &Bounds<ScaledPixels>, inner: &Bounds<ScaledPixels>) -> bool {
    if inner.size.width.0 <= 0.0 || inner.size.height.0 <= 0.0 {
        return true;
    }
    inner.origin.x.0 >= outer.origin.x.0
        && inner.origin.y.0 >= outer.origin.y.0
        && inner.origin.x.0 + inner.size.width.0 <= outer.origin.x.0 + outer.size.width.0
        && inner.origin.y.0 + inner.size.height.0 <= outer.origin.y.0 + outer.size.height.0
}

impl From<Quad> for Primitive {
    fn from(quad: Quad) -> Self {
        Primitive::Quad(quad)
    }
}

#[derive(Debug, Copy, Clone)]
#[repr(C)]
#[expect(missing_docs)]
pub struct Underline {
    pub order: DrawOrder,
    pub pad: u32, // align to 8 bytes
    pub bounds: Bounds<ScaledPixels>,
    pub content_mask: ContentMask<ScaledPixels>,
    pub color: Hsla,
    pub thickness: ScaledPixels,
    pub wavy: PaddedBool32,
}

impl From<Underline> for Primitive {
    fn from(underline: Underline) -> Self {
        Primitive::Underline(underline)
    }
}

#[derive(Debug, Copy, Clone)]
#[repr(C)]
#[expect(missing_docs)]
pub struct Shadow {
    pub order: DrawOrder,
    pub blur_radius: ScaledPixels,
    pub bounds: Bounds<ScaledPixels>,
    pub corner_radii: Corners<ScaledPixels>,
    pub content_mask: ContentMask<ScaledPixels>,
    pub color: Hsla,
    pub element_bounds: Bounds<ScaledPixels>,
    pub element_corner_radii: Corners<ScaledPixels>,
    /// 0 = drop shadow, 1 = inset shadow (rendered inside the element), 2 = drop shadow
    /// rendered outside the element only.
    pub inset: u32,
    pub pad: u32, // align to 8 bytes
}

impl From<Shadow> for Primitive {
    fn from(shadow: Shadow) -> Self {
        Primitive::Shadow(shadow)
    }
}

/// The style of a border.
#[derive(Default, Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[repr(C)]
pub enum BorderStyle {
    /// A solid border.
    #[default]
    Solid = 0,
    /// A dashed border.
    Dashed = 1,
}

/// A data type representing a 2 dimensional transformation that can be applied to an element.
#[derive(Debug, Clone, Copy, PartialEq)]
#[repr(C)]
pub struct TransformationMatrix {
    /// 2x2 matrix containing rotation and scale,
    /// stored row-major
    pub rotation_scale: [[f32; 2]; 2],
    /// translation vector
    pub translation: [f32; 2],
}

impl Eq for TransformationMatrix {}

impl TransformationMatrix {
    /// The unit matrix, has no effect.
    pub fn unit() -> Self {
        Self {
            rotation_scale: [[1.0, 0.0], [0.0, 1.0]],
            translation: [0.0, 0.0],
        }
    }

    /// Move the origin by a given point
    pub fn translate(self, point: Point<ScaledPixels>) -> Self {
        self.compose(Self {
            rotation_scale: [[1.0, 0.0], [0.0, 1.0]],
            translation: [point.x.0, point.y.0],
        })
    }

    /// Clockwise rotation in radians around the origin
    pub fn rotate(self, angle: Radians) -> Self {
        self.compose(Self {
            rotation_scale: [
                [angle.0.cos(), -angle.0.sin()],
                [angle.0.sin(), angle.0.cos()],
            ],
            translation: [0.0, 0.0],
        })
    }

    /// Scale around the origin
    pub fn scale(self, size: Size<f32>) -> Self {
        self.compose(Self {
            rotation_scale: [[size.width, 0.0], [0.0, size.height]],
            translation: [0.0, 0.0],
        })
    }

    /// Perform matrix multiplication with another transformation
    /// to produce a new transformation that is the result of
    /// applying both transformations: first, `other`, then `self`.
    #[inline]
    pub fn compose(self, other: TransformationMatrix) -> TransformationMatrix {
        if other == Self::unit() {
            return self;
        }
        // Perform matrix multiplication
        TransformationMatrix {
            rotation_scale: [
                [
                    self.rotation_scale[0][0] * other.rotation_scale[0][0]
                        + self.rotation_scale[0][1] * other.rotation_scale[1][0],
                    self.rotation_scale[0][0] * other.rotation_scale[0][1]
                        + self.rotation_scale[0][1] * other.rotation_scale[1][1],
                ],
                [
                    self.rotation_scale[1][0] * other.rotation_scale[0][0]
                        + self.rotation_scale[1][1] * other.rotation_scale[1][0],
                    self.rotation_scale[1][0] * other.rotation_scale[0][1]
                        + self.rotation_scale[1][1] * other.rotation_scale[1][1],
                ],
            ],
            translation: [
                self.translation[0]
                    + self.rotation_scale[0][0] * other.translation[0]
                    + self.rotation_scale[0][1] * other.translation[1],
                self.translation[1]
                    + self.rotation_scale[1][0] * other.translation[0]
                    + self.rotation_scale[1][1] * other.translation[1],
            ],
        }
    }

    /// Apply transformation to a point, mainly useful for debugging
    pub fn apply(&self, point: Point<Pixels>) -> Point<Pixels> {
        let input = [point.x.0, point.y.0];
        let mut output = self.translation;
        for (i, output_cell) in output.iter_mut().enumerate() {
            for (k, input_cell) in input.iter().enumerate() {
                *output_cell += self.rotation_scale[i][k] * *input_cell;
            }
        }
        Point::new(output[0].into(), output[1].into())
    }
}

impl Default for TransformationMatrix {
    fn default() -> Self {
        Self::unit()
    }
}

#[derive(Copy, Clone, Debug)]
#[repr(C)]
#[expect(missing_docs)]
pub struct MonochromeSprite {
    pub order: DrawOrder,
    pub pad: u32,
    pub tile: AtlasTile,
    pub bounds: Bounds<ScaledPixels>,
    pub content_mask: ContentMask<ScaledPixels>,
    pub color: Hsla,
    pub transformation: TransformationMatrix,
}

impl From<MonochromeSprite> for Primitive {
    fn from(sprite: MonochromeSprite) -> Self {
        Primitive::MonochromeSprite(sprite)
    }
}

#[derive(Copy, Clone, Debug)]
#[repr(C)]
#[expect(missing_docs)]
pub struct SubpixelSprite {
    pub order: DrawOrder,
    pub pad: u32, // align to 8 bytes
    pub tile: AtlasTile,
    pub bounds: Bounds<ScaledPixels>,
    pub content_mask: ContentMask<ScaledPixels>,
    pub color: Hsla,
    pub transformation: TransformationMatrix,
}

impl From<SubpixelSprite> for Primitive {
    fn from(sprite: SubpixelSprite) -> Self {
        Primitive::SubpixelSprite(sprite)
    }
}

#[derive(Copy, Clone, Debug)]
#[repr(C)]
#[expect(missing_docs)]
pub struct PolychromeSprite {
    pub order: DrawOrder,
    pub pad: u32,
    pub tile: AtlasTile,
    pub grayscale: PaddedBool32,
    pub opacity: f32,
    pub bounds: Bounds<ScaledPixels>,
    pub content_mask: ContentMask<ScaledPixels>,
    pub corner_radii: Corners<ScaledPixels>,
}

impl From<PolychromeSprite> for Primitive {
    fn from(sprite: PolychromeSprite) -> Self {
        Primitive::PolychromeSprite(sprite)
    }
}

#[derive(Clone, Debug)]
#[allow(missing_docs)]
pub struct PaintSurface {
    pub order: DrawOrder,
    pub bounds: Bounds<ScaledPixels>,
    pub content_mask: ContentMask<ScaledPixels>,
    #[cfg(any(target_os = "macos", target_os = "ios"))]
    pub image_buffer: objc2_core_foundation::CFRetained<objc2_core_video::CVPixelBuffer>,
}

/// A blur of what lies below `bounds`, rounded at `corner_radii`: CSS `backdrop-filter: blur()`.
/// `blur_radius` is the blur's standard deviation in device pixels. A renderer that can't read
/// back what it drew skips it, leaving the element's own fill over a sharp backdrop.
#[derive(Clone, Debug, Default)]
#[allow(missing_docs)]
pub struct Backdrop {
    pub order: DrawOrder,
    pub bounds: Bounds<ScaledPixels>,
    pub corner_radii: Corners<ScaledPixels>,
    pub content_mask: ContentMask<ScaledPixels>,
    pub blur_radius: ScaledPixels,
    /// Blurred once when it first shows and kept while it stays in place, so later frames
    /// draw no blur and keep their pass whole; what changes beneath it shows on its next showing.
    pub held: bool,
}

impl Backdrop {
    /// What a held blur is kept against: the same shape, clip and radius in consecutive frames.
    pub fn key(&self) -> [u32; 13] {
        let b = |v: ScaledPixels| v.0.to_bits();
        let m = &self.content_mask.bounds;
        let c = &self.corner_radii;
        [
            b(self.bounds.origin.x), b(self.bounds.origin.y), b(self.bounds.size.width), b(self.bounds.size.height),
            b(m.origin.x), b(m.origin.y), b(m.size.width), b(m.size.height),
            b(c.top_left), b(c.top_right), b(c.bottom_right), b(c.bottom_left),
            b(self.blur_radius),
        ]
    }
}

impl From<Backdrop> for Primitive {
    fn from(backdrop: Backdrop) -> Self {
        Primitive::Backdrop(backdrop)
    }
}

impl From<PaintSurface> for Primitive {
    fn from(surface: PaintSurface) -> Self {
        Primitive::Surface(surface)
    }
}

#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
#[expect(missing_docs)]
pub struct PathId(pub usize);

/// A line made up of a series of vertices and control points.
#[derive(Clone, Debug)]
#[expect(missing_docs)]
pub struct Path<P: Clone + Debug + Default + PartialEq> {
    pub id: PathId,
    pub order: DrawOrder,
    pub bounds: Bounds<P>,
    pub content_mask: ContentMask<P>,
    pub vertices: Vec<PathVertex<P>>,
    pub color: Background,
    start: Point<P>,
    current: Point<P>,
    contour_count: usize,
}

impl Path<Pixels> {
    /// Create a new path with the given starting point.
    pub fn new(start: Point<Pixels>) -> Self {
        Self {
            id: PathId(0),
            order: DrawOrder::default(),
            vertices: Vec::new(),
            start,
            current: start,
            bounds: Bounds {
                origin: start,
                size: Default::default(),
            },
            content_mask: Default::default(),
            color: Default::default(),
            contour_count: 0,
        }
    }

    /// Scale this path by the given factor.
    pub fn scale(&self, factor: f32) -> Path<ScaledPixels> {
        Path {
            id: self.id,
            order: self.order,
            bounds: self.bounds.scale(factor),
            content_mask: self.content_mask.scale(factor),
            vertices: self
                .vertices
                .iter()
                .map(|vertex| vertex.scale(factor))
                .collect(),
            start: self.start.map(|start| start.scale(factor)),
            current: self.current.scale(factor),
            contour_count: self.contour_count,
            color: self.color,
        }
    }

    /// Move the start, current point to the given point.
    pub fn move_to(&mut self, to: Point<Pixels>) {
        self.contour_count += 1;
        self.start = to;
        self.current = to;
    }

    /// Draw a straight line from the current point to the given point.
    pub fn line_to(&mut self, to: Point<Pixels>) {
        self.contour_count += 1;
        if self.contour_count > 1 {
            self.push_triangle(
                (self.start, self.current, to),
                (point(0., 1.), point(0., 1.), point(0., 1.)),
            );
        }
        self.current = to;
    }

    /// Draw a curve from the current point to the given point, using the given control point.
    pub fn curve_to(&mut self, to: Point<Pixels>, ctrl: Point<Pixels>) {
        self.contour_count += 1;
        if self.contour_count > 1 {
            self.push_triangle(
                (self.start, self.current, to),
                (point(0., 1.), point(0., 1.), point(0., 1.)),
            );
        }

        self.push_triangle(
            (self.current, ctrl, to),
            (point(0., 0.), point(0.5, 0.), point(1., 1.)),
        );
        self.current = to;
    }

    /// Push a triangle to the Path.
    pub fn push_triangle(
        &mut self,
        xy: (Point<Pixels>, Point<Pixels>, Point<Pixels>),
        st: (Point<f32>, Point<f32>, Point<f32>),
    ) {
        self.bounds = self
            .bounds
            .union(&Bounds {
                origin: xy.0,
                size: Default::default(),
            })
            .union(&Bounds {
                origin: xy.1,
                size: Default::default(),
            })
            .union(&Bounds {
                origin: xy.2,
                size: Default::default(),
            });

        self.vertices.push(PathVertex {
            xy_position: xy.0,
            st_position: st.0,
            content_mask: Default::default(),
        });
        self.vertices.push(PathVertex {
            xy_position: xy.1,
            st_position: st.1,
            content_mask: Default::default(),
        });
        self.vertices.push(PathVertex {
            xy_position: xy.2,
            st_position: st.2,
            content_mask: Default::default(),
        });
    }
}

impl<T> Path<T>
where
    T: Clone + Debug + Default + PartialEq + PartialOrd + Add<T, Output = T> + Sub<Output = T>,
{
    #[allow(unused)]
    #[expect(missing_docs)]
    pub fn clipped_bounds(&self) -> Bounds<T> {
        self.bounds.intersect(&self.content_mask.bounds)
    }
}

impl From<Path<ScaledPixels>> for Primitive {
    fn from(path: Path<ScaledPixels>) -> Self {
        Primitive::Path(path)
    }
}

#[derive(Clone, Debug)]
#[repr(C)]
#[expect(missing_docs)]
pub struct PathVertex<P: Clone + Debug + Default + PartialEq> {
    pub xy_position: Point<P>,
    pub st_position: Point<f32>,
    pub content_mask: ContentMask<P>,
}

#[expect(missing_docs)]
impl PathVertex<Pixels> {
    pub fn scale(&self, factor: f32) -> PathVertex<ScaledPixels> {
        PathVertex {
            xy_position: self.xy_position.scale(factor),
            st_position: self.st_position,
            content_mask: self.content_mask.scale(factor),
        }
    }
}
