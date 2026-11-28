use std::ops::Range;

use collections::FxHashSet;

use refineable::Refineable;

use crate::{
    AnyElement, App, AvailableSpace, Bounds, ContentMask, Element, ElementId, EntityId,
    GlobalElementId, InspectorElementId, LayoutId, PaintIndex, Pixels, Point,
    PrepaintStateIndex, Size, Style, StyleRefinement, TextStyle, Window,
};

/// A subtree drawn by `build` only when `key` changes, the view drawing it is told its data
/// changed, or the window redraws everything. Otherwise the last drawing is replayed, moved to
/// where the subtree now sits, as a list row that scrolled does. Laid out at `style`, since its
/// contents are not built to measure.
///
/// Hit areas move with the drawing, so listeners must find themselves by hitbox, not by bounds
/// captured when built. A drawing made under the pointer, or with tooltips, deferred draws or
/// text input in it, is replayed only where it was drawn.
pub fn keyed(
    id: impl Into<ElementId>,
    key: u64,
    style: StyleRefinement,
    build: impl FnOnce(&mut Window, &mut App) -> AnyElement + 'static,
) -> Keyed {
    Keyed { id: id.into(), key, style, build: Some(Box::new(build)), measure: None }
}

/// As [`keyed`], for a list item of any height at `width`: its height is measured when built
/// and kept while it replays. Laid out only as a list lays its items out, during prepaint.
pub fn keyed_measured(
    id: impl Into<ElementId>,
    key: u64,
    width: Pixels,
    build: impl FnOnce(&mut Window, &mut App) -> AnyElement + 'static,
) -> Keyed {
    let mut style = StyleRefinement::default();
    style.size.width = Some(width.into());
    Keyed { id: id.into(), key, style, build: Some(Box::new(build)), measure: Some(width) }
}

/// See [`keyed`].
pub struct Keyed {
    id: ElementId,
    key: u64,
    style: StyleRefinement,
    build: Option<Box<dyn FnOnce(&mut Window, &mut App) -> AnyElement>>,
    /// Measured at this width, the height its build lays out to.
    measure: Option<Pixels>,
}

/// A measured build, made while laying out: drawn this frame, not replayed.
pub struct Prebuilt {
    element: AnyElement,
    accessed: FxHashSet<EntityId>,
    moving: bool,
}

struct KeyedState {
    key: u64,
    /// The draw that made `prepaint_range` and `paint_range`: only the last one's still stand.
    drawn: u64,
    bounds: Bounds<Pixels>,
    outer: ContentMask<Pixels>,
    text_style: TextStyle,
    opacity: f32,
    prepaint_range: Range<PrepaintStateIndex>,
    paint_range: Range<PaintIndex>,
    accessed_entities: FxHashSet<EntityId>,
    /// Drawn with the pointer over it, so its hover styles may be stale anywhere else.
    hovered: bool,
    /// `bounds` and every hit area drawn in it, where the pointer can hover it.
    extent: Bounds<Pixels>,
    /// Holds what cannot move: tooltips, deferred draws, text input.
    pinned: bool,
    /// Asked for another frame while drawn: it is animating, so it draws afresh.
    moving: bool,
    /// What it told of their place, in the frame it was drawn in.
    places: Range<usize>,
}

/// `element` filling `size`, as it would in the slot it was laid out for: as a layout root of
/// its own, a box with no width shrinks to its content.
fn sized(element: AnyElement, size: Size<Option<Pixels>>) -> AnyElement {
    use crate::{IntoElement, ParentElement, Styled};
    let mut boxed = crate::div().flex().flex_col();
    if let Some(w) = size.width {
        boxed = boxed.w(w);
    }
    if let Some(h) = size.height {
        boxed = boxed.h(h);
    }
    boxed.child(element).into_any_element()
}

/// How this frame draws a keyed subtree.
pub enum KeyedDraw {
    /// Built afresh.
    Built(AnyElement),
    /// Replayed from the last frame, moved by this much.
    Replayed(Point<Pixels>),
    /// Drawn plainly, outside the cache (under a zoom, or while the inspector picks).
    Plain(AnyElement),
}

impl Element for Keyed {
    type RequestLayoutState = Option<Prebuilt>;
    type PrepaintState = Option<KeyedDraw>;

    fn id(&self) -> Option<ElementId> {
        Some(self.id.clone())
    }

    fn source_location(&self) -> Option<&'static core::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, Option<Prebuilt>) {
        let mut style = Style::default();
        style.refine(&self.style);
        let Some(width) = self.measure else {
            return (window.request_layout(style, None, cx), None);
        };
        let (key, draw_count) = (self.key, window.draw_count);
        let kept = window.keyed_may_replay()
            && id
                .and_then(|id| {
                    window.peek_element_state::<KeyedState, _>(id, |s| {
                        (s.key == key && s.drawn + 1 == draw_count && !s.moving && s.bounds.size.width == width)
                            .then_some(s.bounds.size.height)
                    })
                })
                .flatten()
                .is_some_and(|height| {
                    style.size.height = height.into();
                    true
                });
        if kept {
            return (window.request_layout(style, None, cx), None);
        }
        let Some(build) = self.build.take() else {
            return (window.request_layout(style, None, cx), None);
        };
        let (asks, volatile) = (window.keyed_asks.get(), crate::keyed_volatile_count());
        let ((element, size), accessed) = window.keyed_root(|window| {
            cx.detect_accessed_entities(|cx| {
                let mut element = sized(build(window, cx), Size { width: Some(width), height: None });
                let space = Size { width: AvailableSpace::Definite(width), height: AvailableSpace::MinContent };
                let size = element.layout_as_root(space, window, cx);
                (element, size)
            })
        });
        let moving = window.keyed_asks.get() != asks || crate::keyed_volatile_count() != volatile;
        style.size.height = size.height.into();
        (window.request_layout(style, None, cx), Some(Prebuilt { element, accessed, moving }))
    }

    fn prepaint(
        &mut self,
        id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        prebuilt: &mut Option<Prebuilt>,
        window: &mut Window,
        cx: &mut App,
    ) -> Option<KeyedDraw> {
        let prebuilt = prebuilt.take();
        let build = self.build.take();
        if prebuilt.is_none() && build.is_none() {
            return None;
        }
        if window.element_zoom.is_some() || window.is_inspector_picking(cx) {
            let mut element = match (prebuilt, build) {
                (Some(p), _) => p.element,
                (None, Some(build)) => {
                    let mut element = sized(build(window, cx), bounds.size.map(Some));
                    element.layout_as_root(Size::<AvailableSpace>::from(bounds.size), window, cx);
                    element
                }
                (None, None) => return None,
            };
            element.prepaint_at(bounds.origin, window, cx);
            return Some(KeyedDraw::Plain(element));
        }
        let key = self.key;
        let outer = window.keyed_clip();
        let text_style = window.text_style();
        let opacity = window.element_opacity();
        let mouse = window.mouse_position();
        let may_replay = window.keyed_may_replay();
        let scale = window.scale_factor();
        window.with_element_state::<KeyedState, _>(id.unwrap(), |state, window| {
            if let Some(mut state) = state.filter(|_| prebuilt.is_none()) {
                let by = bounds.origin - state.bounds.origin;
                // Text sits on whole device pixels; a part-pixel move would blur it.
                let whole = |v: Pixels| ((v.0 * scale) - (v.0 * scale).round()).abs() < 1e-3;
                let moved = by != Point::default();
                if may_replay
                    && state.drawn + 1 == window.draw_count
                    && state.key == key
                    && state.bounds.size == bounds.size
                    && state.outer == outer
                    && state.text_style == text_style
                    && state.opacity.to_bits() == opacity.to_bits()
                    && !state.hovered
                    && !state.moving
                    && !state.extent.contains(&(mouse - by))
                    && (!moved || (!state.pinned && whole(by.x) && whole(by.y)))
                {
                    let start = window.prepaint_index();
                    let places = window.places_index();
                    window.keyed_outer.push(outer.clone());
                    window.reuse_prepaint_shifted(state.prepaint_range.clone(), by);
                    window.replay_places(state.places.clone(), by);
                    window.keyed_outer.pop();
                    state.places = places..window.places_index();
                    cx.entities.extend_accessed(state.accessed_entities.iter().copied());
                    state.prepaint_range = start..window.prepaint_index();
                    state.bounds = bounds;
                    state.extent.origin = state.extent.origin + by;
                    state.drawn = window.draw_count;
                    crate::draw_stats::keyed(true);
                    return (Some(KeyedDraw::Replayed(by)), state);
                }
            }
            crate::draw_stats::keyed(false);
            let start = window.prepaint_index();
            let places = window.places_index();
            let (asks, volatile) = (window.keyed_asks.get(), crate::keyed_volatile_count());
            let mut moving = prebuilt.as_ref().is_some_and(|p| p.moving);
            let mut accessed_entities = FxHashSet::default();
            let (prebuilt, build) = (prebuilt.map(|p| {
                accessed_entities = p.accessed;
                p.element
            }), build);
            window.keyed_outer.push(outer.clone());
            let (element, accessed) = window.keyed_root(|window| {
                cx.detect_accessed_entities(|cx| {
                    let mut element = match (prebuilt, build) {
                        (Some(element), _) => element,
                        (None, Some(build)) => {
                            let mut element = sized(build(window, cx), bounds.size.map(Some));
                            element.layout_as_root(Size::<AvailableSpace>::from(bounds.size), window, cx);
                            element
                        }
                        (None, None) => unreachable!("checked above"),
                    };
                    element.prepaint_at(bounds.origin, window, cx);
                    element
                })
            });
            accessed_entities.extend(accessed);
            window.keyed_outer.pop();
            moving |= window.keyed_asks.get() != asks || crate::keyed_volatile_count() != volatile;
            let end = window.prepaint_index();
            let pinned = window.prepaint_pins(&start, &end);
            let extent = window.prepaint_extent(bounds, &start, &end);
            let state = KeyedState {
                key,
                drawn: window.draw_count,
                bounds,
                outer,
                text_style,
                opacity,
                prepaint_range: start..end,
                paint_range: PaintIndex::default()..PaintIndex::default(),
                accessed_entities,
                hovered: extent.contains(&mouse),
                extent,
                pinned,
                moving,
                places: places..window.places_index(),
            };
            (Some(KeyedDraw::Built(element)), state)
        })
    }

    fn paint(
        &mut self,
        id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        _bounds: Bounds<Pixels>,
        _request_layout: &mut Option<Prebuilt>,
        draw: &mut Option<KeyedDraw>,
        window: &mut Window,
        cx: &mut App,
    ) {
        let Some(draw) = draw.as_mut() else { return };
        if let KeyedDraw::Plain(element) = draw {
            element.paint(window, cx);
            return;
        }
        window.with_element_state::<KeyedState, _>(id.unwrap(), |state, window| {
            let mut state = state.expect("a keyed subtree prepaints before it paints");
            let outer = state.outer.clone();
            window.keyed_paint(outer, |window| {
                let start = window.paint_index();
                match draw {
                    KeyedDraw::Built(element) => {
                        let (asks, volatile) = (window.keyed_asks.get(), crate::keyed_volatile_count());
                        let ((), accessed) = cx.detect_accessed_entities(|cx| element.paint(window, cx));
                        state.accessed_entities.extend(accessed);
                        let end = window.paint_index();
                        state.pinned |= window.paint_pins(&start, &end);
                        state.moving |= window.keyed_asks.get() != asks || crate::keyed_volatile_count() != volatile;
                    }
                    KeyedDraw::Replayed(by) => {
                        if *by == Point::default() {
                            window.reuse_paint(state.paint_range.clone());
                        } else {
                            window.reuse_paint_shifted(state.paint_range.clone(), *by);
                        }
                    }
                    KeyedDraw::Plain(_) => {}
                }
                state.paint_range = start..window.paint_index();
            });
            ((), state)
        })
    }
}
