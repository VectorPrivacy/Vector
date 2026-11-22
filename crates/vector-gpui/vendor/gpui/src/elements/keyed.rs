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
    Keyed { id: id.into(), key, style, build: Some(Box::new(build)) }
}

/// See [`keyed`].
pub struct Keyed {
    id: ElementId,
    key: u64,
    style: StyleRefinement,
    build: Option<Box<dyn FnOnce(&mut Window, &mut App) -> AnyElement>>,
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
    /// Holds what cannot move: tooltips, deferred draws, text input.
    pinned: bool,
    /// Asked for another frame while drawn: it is animating, so it draws afresh.
    moving: bool,
}

/// `element` filling `size`, as it would in the slot it was laid out for: as a layout root of
/// its own, a box with no width shrinks to its content.
fn sized(element: AnyElement, size: Size<Pixels>) -> AnyElement {
    use crate::{IntoElement, ParentElement, Styled};
    crate::div().w(size.width).h(size.height).flex().flex_col().child(element).into_any_element()
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
    type RequestLayoutState = ();
    type PrepaintState = Option<KeyedDraw>;

    fn id(&self) -> Option<ElementId> {
        Some(self.id.clone())
    }

    fn source_location(&self) -> Option<&'static core::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, ()) {
        let mut style = Style::default();
        style.refine(&self.style);
        (window.request_layout(style, None, cx), ())
    }

    fn prepaint(
        &mut self,
        id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _request_layout: &mut (),
        window: &mut Window,
        cx: &mut App,
    ) -> Option<KeyedDraw> {
        let build = self.build.take()?;
        if window.element_zoom.is_some() || window.is_inspector_picking(cx) {
            let mut element = sized(build(window, cx), bounds.size);
            element.layout_as_root(Size::<AvailableSpace>::from(bounds.size), window, cx);
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
            if let Some(mut state) = state {
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
                    && !bounds.contains(&mouse)
                    && (!moved || (!state.pinned && whole(by.x) && whole(by.y)))
                {
                    let start = window.prepaint_index();
                    window.keyed_outer.push(outer.clone());
                    window.reuse_prepaint_shifted(state.prepaint_range.clone(), by);
                    window.keyed_outer.pop();
                    cx.entities.extend_accessed(state.accessed_entities.iter().copied());
                    state.prepaint_range = start..window.prepaint_index();
                    state.bounds = bounds;
                    state.drawn = window.draw_count;
                    crate::draw_stats::keyed(true);
                    return (Some(KeyedDraw::Replayed(by)), state);
                }
            }
            crate::draw_stats::keyed(false);
            let start = window.prepaint_index();
            let asks = window.keyed_asks.get();
            window.keyed_outer.push(outer.clone());
            let (element, accessed_entities) = window.keyed_root(|window| {
                cx.detect_accessed_entities(|cx| {
                    let mut element = sized(build(window, cx), bounds.size);
                    element.layout_as_root(Size::<AvailableSpace>::from(bounds.size), window, cx);
                    element.prepaint_at(bounds.origin, window, cx);
                    element
                })
            });
            window.keyed_outer.pop();
            let end = window.prepaint_index();
            let pinned = window.prepaint_pins(&start, &end);
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
                hovered: bounds.contains(&mouse),
                pinned,
                moving: window.keyed_asks.get() != asks,
            };
            (Some(KeyedDraw::Built(element)), state)
        })
    }

    fn paint(
        &mut self,
        id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        _bounds: Bounds<Pixels>,
        _request_layout: &mut (),
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
                        let asks = window.keyed_asks.get();
                        let ((), accessed) = cx.detect_accessed_entities(|cx| element.paint(window, cx));
                        state.accessed_entities.extend(accessed);
                        let end = window.paint_index();
                        state.pinned |= window.paint_pins(&start, &end);
                        state.moving |= window.keyed_asks.get() != asks;
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
