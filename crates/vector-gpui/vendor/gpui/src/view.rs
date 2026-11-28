use crate::{
    AnyElement, AnyEntity, AnyWeakEntity, App, AvailableSpace, Bounds, ContentMask, Context,
    Element, ElementId, Entity, EntityId, GlobalElementId, InspectorElementId, IntoElement,
    LayoutId, PaintIndex, Pixels, Point, PrepaintStateIndex, Render, RenderOnce, Size, Style,
    StyleRefinement, TextStyle, WeakEntity,
};
use crate::{Empty, Window};
use crate::key_dispatch::{DispatchNodeId, OpenReuse};
use crate::window::DrawCtx;
use std::rc::Rc;
use anyhow::Result;
use collections::FxHashSet;
use refineable::Refineable;
use std::mem;
use std::{
    any::{TypeId, type_name},
    fmt,
    ops::Range,
};

/// A dynamically-typed view handle that can be downcast to a specific `Entity<V>`.
///
/// This is the type-erased counterpart to [`ViewElement`]: it holds an entity plus
/// a function pointer to its render, and is itself a [`View`], so embedding it as an
/// element goes through the same [`ViewElement`] machinery as any other view.
#[derive(Clone, Debug)]
pub struct AnyView {
    entity: AnyEntity,
    render: fn(&AnyView, &mut Window, &mut App) -> AnyElement,
    view_type_name: &'static str,
}

impl<V: Render> From<Entity<V>> for AnyView {
    fn from(value: Entity<V>) -> Self {
        AnyView {
            entity: value.into_any(),
            render: any_view::render::<V>,
            view_type_name: type_name::<V>(),
        }
    }
}

impl AnyView {
    /// Embed this view as a cached [`ViewElement`] laid out at `style`.
    ///
    /// The rendered subtree is recycled from the previous frame unless
    /// [Context::notify] was called on the backing entity since it was rendered
    /// (or [Window::refresh] is called, which ignores caching).
    pub fn cached(self, style: StyleRefinement) -> ViewElement<AnyView> {
        let view = self.clone();
        let mut element = self.into_element().cached(style);
        element.respawn = Some(Rc::new(move |window: &mut Window, cx: &mut App| (view.render)(&view, window, cx)));
        element
    }

    /// Convert this to a weak handle.
    pub fn downgrade(&self) -> AnyWeakView {
        AnyWeakView {
            entity: self.entity.downgrade(),
            render: self.render,
            view_type_name: self.view_type_name,
        }
    }

    /// Convert this to a [Entity] of a specific type.
    /// If this handle does not contain a view of the specified type, returns itself in an `Err` variant.
    pub fn downcast<T: 'static>(self) -> Result<Entity<T>, Self> {
        match self.entity.downcast() {
            Ok(entity) => Ok(entity),
            Err(entity) => Err(Self {
                entity,
                render: self.render,
                view_type_name: self.view_type_name,
            }),
        }
    }

    /// Gets the [TypeId] of the underlying view.
    pub fn entity_type(&self) -> TypeId {
        self.entity.entity_type
    }

    /// The [`EntityId`] of this view.
    pub fn entity_id(&self) -> EntityId {
        self.entity.entity_id()
    }
}

impl PartialEq for AnyView {
    fn eq(&self, other: &Self) -> bool {
        self.entity == other.entity
    }
}

impl Eq for AnyView {}

/// `AnyView` is the type-erased [`View`]: its `render` is a function pointer rather
/// than a concrete type, but it participates in the reactive graph exactly like any
/// other view via [`ViewElement`].
impl View for AnyView {
    fn entity_id(&self) -> Option<EntityId> {
        Some(self.entity.entity_id())
    }

    #[cfg(not(gpui_hot_reload))]
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        (self.render)(&self, window, cx)
    }

    // The stored function pointer was captured from the original binary, so it must be looked up
    // in the jump table to reach the patched render.
    #[cfg(gpui_hot_reload)]
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        subsecond::HotFn::current(self.render).call((&self, window, cx))
    }
}

impl<V: 'static + Render> IntoElement for Entity<V> {
    type Element = ViewElement<Entity<V>>;

    fn into_element(self) -> Self::Element {
        ViewElement::with_type_name(self, type_name::<V>())
    }

    #[inline(never)]
    fn into_any_element(self) -> AnyElement {
        self.into_element().into_any()
    }
}

impl IntoElement for AnyView {
    type Element = ViewElement<AnyView>;

    fn into_element(self) -> Self::Element {
        let view_type_name = self.view_type_name;
        ViewElement::with_type_name(self, view_type_name)
    }
}

/// A weak, dynamically-typed view handle.
pub struct AnyWeakView {
    entity: AnyWeakEntity,
    render: fn(&AnyView, &mut Window, &mut App) -> AnyElement,
    view_type_name: &'static str,
}

impl AnyWeakView {
    /// Upgrade to a strong `AnyView` handle, if the view is still alive.
    pub fn upgrade(&self) -> Option<AnyView> {
        let entity = self.entity.upgrade()?;
        Some(AnyView {
            entity,
            render: self.render,
            view_type_name: self.view_type_name,
        })
    }
}

impl<V: 'static + Render> From<WeakEntity<V>> for AnyWeakView {
    fn from(view: WeakEntity<V>) -> Self {
        AnyWeakView {
            entity: view.into(),
            render: any_view::render::<V>,
            view_type_name: type_name::<V>(),
        }
    }
}

impl PartialEq for AnyWeakView {
    fn eq(&self, other: &Self) -> bool {
        self.entity == other.entity
    }
}

impl std::fmt::Debug for AnyWeakView {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AnyWeakView")
            .field("entity_id", &self.entity.entity_id)
            .finish_non_exhaustive()
    }
}

mod any_view {
    use crate::{AnyElement, AnyView, App, IntoElement, Render, Window};

    pub(crate) fn render<V: 'static + Render>(
        view: &AnyView,
        window: &mut Window,
        cx: &mut App,
    ) -> AnyElement {
        let view = view.clone().downcast::<V>().unwrap();
        // Record the view's Render type name so the accessibility debug dump can
        // attribute nodes to the view that produced them.
        #[cfg(debug_assertions)]
        window
            .a11y
            .view_type_names
            .insert(view.entity_id(), std::any::type_name::<V>());
        view.update(cx, |view, cx| view.render(window, cx).into_any_element())
    }
}

/// A renderable that participates in GPUI's reactive graph — the unifying model
/// behind [`Render`] and [`RenderOnce`].
///
/// When `entity_id()` returns `Some`, that id becomes the view's identity: it gets
/// a unique element-id space (so internal `use_state` / `.id(..)` never collide
/// across siblings) and `cx.notify()` on that entity re-renders only this view's
/// subtree. `None` behaves like a stateless component.
///
/// You rarely implement `View` directly. `Entity<T: Render>` and any `T: RenderOnce`
/// get a blanket impl below; implement it by hand only when a component needs both
/// parent-supplied props *and* a backing entity for identity.
pub trait View: 'static + Sized {
    /// This view's identity, if it has one. A view typically holds the backing
    /// entity as a field and returns its [`EntityId`] here.
    ///
    /// The id becomes this view's [`ElementId`], so two views keyed on the same
    /// entity must not be rendered at the same position in the element tree
    /// (e.g. as siblings under the same parent): their internal element state
    /// (`use_state`, scroll offsets, etc.) would silently collide. Nesting is
    /// fine — the id is scoped by the parent path.
    fn entity_id(&self) -> Option<EntityId>;

    /// Render this view into an element tree, consuming `self`.
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement;
}

/// A stateless component (`RenderOnce`) is a `View` with no identity.
impl<T: RenderOnce> View for T {
    fn entity_id(&self) -> Option<EntityId> {
        None
    }

    #[inline]
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        RenderOnce::render(self, window, cx)
    }
}

/// An entity that renders itself (`Render`) is a `View` keyed on its own id.
impl<T: Render> View for Entity<T> {
    fn entity_id(&self) -> Option<EntityId> {
        Some(Entity::entity_id(self))
    }

    #[inline]
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        self.update(cx, |this, cx| {
            Render::render(this, window, cx).into_any_element()
        })
    }
}

impl<T: Render> Entity<T> {
    /// Embed this entity as a cached [`ViewElement`] laid out at `style`.
    ///
    /// The rendered subtree is reused until the entity is notified (or the
    /// cached bounds / text style change). Caching requires a definite size:
    /// a cached view is laid out from `style` and is *not* measured from its
    /// contents. Use [`ViewElement::new`] (or `.child(entity)`) for the
    /// uncached case.
    #[track_caller]
    pub fn cached(self, style: StyleRefinement) -> ViewElement<Entity<T>> {
        let view = self.clone();
        let mut element = ViewElement::with_type_name(self, type_name::<T>()).cached(style);
        element.respawn = Some(Rc::new(move |window: &mut Window, cx: &mut App| render_view(view.clone(), window, cx)));
        element
    }
}

/// The element type for [`View`] implementations. Wraps a `View` and hooks it
/// into layout, prepaint, and paint. Constructed via [`ViewElement::new`].
#[doc(hidden)]
pub struct ViewElement<V: View> {
    view: Option<V>,
    entity_id: Option<EntityId>,
    cached_style: Option<StyleRefinement>,
    /// Renders the view again on its own, for a cached view drawn alone in its parent's reuse.
    respawn: Option<Respawn>,
    view_type_name: &'static str,
    #[cfg(debug_assertions)]
    source: &'static core::panic::Location<'static>,
}

pub(crate) type Respawn = Rc<dyn Fn(&mut Window, &mut App) -> AnyElement>;

/// A cached view as drawn inside another: what it takes to draw it alone there again.
/// Ranges are in the frame the holder's own ranges are from.
#[derive(Clone)]
pub(crate) struct ViewSlot {
    entity_id: EntityId,
    global_id: GlobalElementId,
    respawn: Respawn,
    prepaint_range: Range<PrepaintStateIndex>,
    paint_range: Range<PaintIndex>,
    /// The dispatch node it was drawn under.
    parent_node: Option<DispatchNodeId>,
}

impl<V: View> ViewElement<V> {
    /// Wrap a [`View`] as an element.
    #[track_caller]
    pub fn new(view: V) -> Self {
        Self::with_type_name(view, type_name::<V>())
    }

    /// Wraps a view whose draw time profiling attributes to `view_type_name`,
    /// such as an entity's type rather than [`Entity`]'s.
    #[track_caller]
    pub(crate) fn with_type_name(view: V, view_type_name: &'static str) -> Self {
        let entity_id = view.entity_id();
        ViewElement {
            entity_id,
            cached_style: None,
            respawn: None,
            view_type_name,
            view: Some(view),
            #[cfg(debug_assertions)]
            source: core::panic::Location::caller(),
        }
    }

    /// Enable caching of this view's rendered subtree, laid out at `style`.
    /// The composer supplies the layout style because caching skips rendering
    /// the contents to measure them.
    ///
    /// Crate-private on purpose: caching is only sound for entity-backed views,
    /// where [`Context::notify`] is the contract that busts the cache. A stateless
    /// view has no such contract, so a frozen subtree could never be invalidated.
    /// Reach this through [`Entity::cached`] or [`AnyView::cached`], which are
    /// entity-backed by construction.
    pub(crate) fn cached(mut self, style: StyleRefinement) -> Self {
        self.cached_style = Some(style);
        self
    }
}

struct ViewElementState {
    prepaint_range: Range<PrepaintStateIndex>,
    paint_range: Range<PaintIndex>,
    cache_key: ViewElementCacheKey,
    /// The entities the view accessed while it was last drawn, in any phase,
    /// restored when its prepaint is reused so the window stays subscribed.
    accessed_entities: FxHashSet<EntityId>,
    /// The draw these ranges are from: a parent reused whole leaves its views' ranges behind.
    drawn: u64,
    /// The cached views drawn inside it, in drawing order.
    slots: Vec<ViewSlot>,
    /// Whether every slot's paint range was found, so a paint can reuse around them.
    slots_painted: bool,
    /// This frame's views drawn afresh inside a reuse, with what prepaint made of each.
    holes: Vec<(usize, Option<AnyElement>, Range<PaintIndex>)>,
    punched: bool,
    prepaint_ctx: Option<Rc<DrawCtx>>,
    paint_ctx: Option<Rc<DrawCtx>>,
}

fn move_slots(slots: &mut [ViewSlot], from: &PrepaintStateIndex, to: &PrepaintStateIndex) {
    for slot in slots {
        slot.parent_node = slot.parent_node.map(|n| n.moved(from.dispatch_index(), to.dispatch_index()));
        slot.prepaint_range = slot.prepaint_range.start.moved(from, to)..slot.prepaint_range.end.moved(from, to);
    }
}

fn move_slot_paints(slots: &mut [ViewSlot], from: &PaintIndex, to: &PaintIndex) {
    for slot in slots {
        slot.paint_range = slot.paint_range.start.moved(from, to)..slot.paint_range.end.moved(from, to);
    }
}

struct ViewElementCacheKey {
    bounds: Bounds<Pixels>,
    content_mask: ContentMask<Pixels>,
    text_style: TextStyle,
    opacity: f32,
    zoom: Option<(Point<Pixels>, f32)>,
}

#[cfg(not(gpui_hot_reload))]
#[inline]
fn render_view<V: View>(view: V, window: &mut Window, cx: &mut App) -> AnyElement {
    view.render(window, cx).into_any_element()
}

/// Every view and component render passes through this hot-patchable boundary, so the
/// patched code for that render and everything it calls runs on the next frame.
#[cfg(gpui_hot_reload)]
fn render_view<V: View>(view: V, window: &mut Window, cx: &mut App) -> AnyElement {
    #[inline(never)]
    fn render_view_impl<V: View>(view: V, window: &mut Window, cx: &mut App) -> AnyElement {
        view.render(window, cx).into_any_element()
    }

    subsecond::HotFn::current(render_view_impl::<V>).call((view, window, cx))
}

impl<V: View> Element for ViewElement<V> {
    type RequestLayoutState = Option<AnyElement>;
    type PrepaintState = Option<AnyElement>;

    fn id(&self) -> Option<ElementId> {
        self.entity_id.map(ElementId::View)
    }

    fn source_location(&self) -> Option<&'static core::panic::Location<'static>> {
        #[cfg(debug_assertions)]
        return Some(self.source);

        #[cfg(not(debug_assertions))]
        return None;
    }

    fn request_layout(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, Self::RequestLayoutState) {
        if let Some(entity_id) = self.entity_id {
            // Stateful path: create a reactive boundary.
            #[cfg(feature = "profiler")]
            let start = window.window_profiler.begin_view();
            let view = &mut self.view;
            let layout = request_layout_view(
                entity_id,
                self.cached_style.as_ref(),
                window,
                cx,
                &mut |window, cx| render_view(view.take().unwrap(), window, cx),
            );
            #[cfg(feature = "profiler")]
            window.window_profiler.end_view(
                start,
                self.view_type_name,
                crate::profiler::ViewPhase::RequestLayout,
            );
            layout
        } else {
            // Stateless path: isolate subtree via type name (no entity identity).
            request_layout_component(type_name::<V>(), window, cx, &mut |window, cx| {
                render_view(self.view.take().unwrap(), window, cx)
            })
        }
    }

    fn prepaint(
        &mut self,
        global_id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        element: &mut Self::RequestLayoutState,
        window: &mut Window,
        cx: &mut App,
    ) -> Option<AnyElement> {
        if let Some(entity_id) = self.entity_id {
            // Stateful path.
            #[cfg(feature = "profiler")]
            let start = window.window_profiler.begin_view();
            let timed = crate::draw_stats::views_on().then(std::time::Instant::now);
            let prepainted = prepaint_view(
                entity_id,
                global_id,
                bounds,
                element,
                window,
                cx,
                &mut |window, cx| render_view(self.view.take().unwrap(), window, cx),
                self.respawn.as_ref(),
            );
            if let Some(at) = timed {
                crate::draw_stats::view(self.view_type_name, entity_id.as_u64(), 0, at.elapsed());
            }
            #[cfg(feature = "profiler")]
            window.window_profiler.end_view(
                start,
                self.view_type_name,
                crate::profiler::ViewPhase::Prepaint,
            );
            prepainted
        } else {
            // Stateless path: just prepaint the element.
            prepaint_component(type_name::<V>(), element, window, cx)
        }
    }

    fn paint(
        &mut self,
        global_id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        _bounds: Bounds<Pixels>,
        _request_layout: &mut Self::RequestLayoutState,
        element: &mut Self::PrepaintState,
        window: &mut Window,
        cx: &mut App,
    ) {
        if let Some(entity_id) = self.entity_id {
            // Stateful path.
            #[cfg(feature = "profiler")]
            let start = window.window_profiler.begin_view();
            let timed = crate::draw_stats::views_on().then(std::time::Instant::now);
            paint_view(
                entity_id,
                self.cached_style.is_some(),
                global_id,
                element,
                window,
                cx,
            );
            if let Some(at) = timed {
                crate::draw_stats::view(self.view_type_name, entity_id.as_u64(), 1, at.elapsed());
            }
            #[cfg(feature = "profiler")]
            window.window_profiler.end_view(
                start,
                self.view_type_name,
                crate::profiler::ViewPhase::Paint,
            );
        } else {
            // Stateless path: just paint the element.
            paint_component(std::any::type_name::<V>(), element, window, cx);
        }
    }
}

/// A view that renders nothing
pub struct EmptyView;

impl Render for EmptyView {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        Empty
    }
}

#[inline(never)]
fn request_layout_view(
    entity_id: EntityId,
    cached_style: Option<&StyleRefinement>,
    window: &mut Window,
    cx: &mut App,
    render: &mut dyn FnMut(&mut Window, &mut App) -> AnyElement,
) -> (LayoutId, Option<AnyElement>) {
    window.with_rendered_view(entity_id, |window| {
        let caching_disabled = window.is_inspector_picking(cx);
        match cached_style {
            Some(style) if !caching_disabled => {
                let mut root_style = Style::default();
                root_style.refine(style);
                let layout_id = window.request_layout(root_style, None, cx);
                (layout_id, None)
            }
            _ => {
                let mut element = render(window, cx);
                let layout_id = element.request_layout(window, cx);
                (layout_id, Some(element))
            }
        }
    })
}

#[inline(never)]
fn request_layout_component(
    name: &'static str,
    window: &mut Window,
    cx: &mut App,
    render: &mut dyn FnMut(&mut Window, &mut App) -> AnyElement,
) -> (LayoutId, Option<AnyElement>) {
    window.with_id(ElementId::from(name), |window| {
        let mut element = render(window, cx);
        let layout_id = element.request_layout(window, cx);
        (layout_id, Some(element))
    })
}

#[inline(never)]
fn prepaint_view(
    entity_id: EntityId,
    global_id: Option<&GlobalElementId>,
    bounds: Bounds<Pixels>,
    element: &mut Option<AnyElement>,
    window: &mut Window,
    cx: &mut App,
    render: &mut dyn FnMut(&mut Window, &mut App) -> AnyElement,
    respawn: Option<&Respawn>,
) -> Option<AnyElement> {
    // Ranges handed down by a parent drawing this view alone inside its own reuse.
    let given = window.punch_from.take();
    window.set_view_id(entity_id);
    if let Some(mut element) = element.take() {
        return window.with_rendered_view(entity_id, |window| {
            element.prepaint(window, cx);
            Some(element)
        });
    }
    let parent_node = window.next_frame.dispatch_tree.active_node_id();
    let ctx = window.draw_ctx();
    let start = window.prepaint_index();
    let prepainted = window.with_rendered_view(entity_id, |window| {
        window.with_element_state::<ViewElementState, _>(
            global_id.unwrap(),
            |element_state, window| {
                let content_mask = window.content_mask();
                let text_style = window.text_style();
                let opacity = window.element_opacity();
                let zoom = window.element_zoom;
                let draw_count = window.draw_count;
                // The paint stacks hold until it paints, which a parent's reuse reads first.
                let paint_ctx = element_state.as_ref().and_then(|s| s.paint_ctx.clone());

                // Opacity is compared by bits so that a NaN opacity still matches itself
                // and the view keeps using its cache.
                if let Some(mut element_state) = element_state
                    && element_state.cache_key.bounds == bounds
                    && element_state.cache_key.content_mask == content_mask
                    && element_state.cache_key.text_style == text_style
                    && element_state.cache_key.opacity.to_bits() == opacity.to_bits()
                    && element_state.cache_key.zoom == zoom
                    && !window.refreshing
                    && (given.is_some() || element_state.drawn + 1 == draw_count)
                {
                    let base = element_state.prepaint_range.start.clone();
                    let old = given.clone().unwrap_or_else(|| element_state.prepaint_range.clone());
                    if !window.dirty_views.contains(&entity_id) {
                        let prepaint_start = window.prepaint_index();
                        window.reuse_prepaint(old);
                        cx.entities
                            .extend_accessed(element_state.accessed_entities.iter().copied());
                        let prepaint_end = window.prepaint_index();
                        move_slots(&mut element_state.slots, &base, &prepaint_start);
                        element_state.prepaint_range = prepaint_start..prepaint_end;
                        element_state.drawn = draw_count;
                        element_state.punched = false;
                        element_state.holes.clear();
                        element_state.prepaint_ctx = Some(Rc::new(ctx));
                        return (None, element_state);
                    }
                    if window.may_punch(entity_id, cx)
                        && let Some(holes) = punch_holes(&element_state, entity_id, &base, &old, window)
                    {
                        punch_prepaint(&mut element_state, holes, &base, old, window, cx);
                        element_state.drawn = draw_count;
                        element_state.prepaint_ctx = Some(Rc::new(ctx));
                        return (None, element_state);
                    }
                }

                let inside = window.refreshes_inside(entity_id);
                let refreshing = mem::replace(&mut window.refreshing, inside);
                let prepaint_start = window.prepaint_index();
                window.view_slots.push(Vec::new());
                let (element, accessed_entities) = cx.detect_accessed_entities(|cx| {
                    let mut element = render(window, cx);
                    element.layout_as_root(Size::<AvailableSpace>::from(bounds.size), window, cx);
                    element.prepaint_at(bounds.origin, window, cx);
                    element
                });
                let slots = window.view_slots.pop().unwrap_or_default();

                let prepaint_end = window.prepaint_index();
                window.refreshing = refreshing;

                (
                    Some(element),
                    ViewElementState {
                        accessed_entities,
                        prepaint_range: prepaint_start..prepaint_end,
                        paint_range: PaintIndex::default()..PaintIndex::default(),
                        cache_key: ViewElementCacheKey {
                            bounds,
                            content_mask,
                            text_style,
                            opacity,
                            zoom,
                        },
                        drawn: draw_count,
                        slots,
                        slots_painted: false,
                        holes: Vec::new(),
                        punched: false,
                        prepaint_ctx: Some(Rc::new(ctx)),
                        paint_ctx,
                    },
                )
            },
        )
    });
    let end = window.prepaint_index();
    if let (Some(respawn), Some(global_id), Some(holder)) = (respawn, global_id, window.view_slots.last_mut()) {
        holder.push(ViewSlot {
            entity_id,
            global_id: global_id.clone(),
            respawn: respawn.clone(),
            prepaint_range: start..end,
            paint_range: PaintIndex::default()..PaintIndex::default(),
            parent_node,
        });
    }
    prepainted
}

/// The slots a view dirty only for views inside it draws afresh, reusing the rest: `None`
/// when that cannot be done (a dirty view it holds is not one of them, the slots' paint
/// ranges are unknown, or a reused deferred draw hangs off a node the reuse does not carry).
fn punch_holes(
    state: &ViewElementState,
    entity_id: EntityId,
    base: &PrepaintStateIndex,
    old: &Range<PrepaintStateIndex>,
    window: &Window,
) -> Option<Vec<usize>> {
    if !state.slots_painted || state.slots.is_empty() {
        return None;
    }
    let inside = window.dirty_inside(entity_id)?;
    if inside.is_empty() {
        return None;
    }
    let holes: Vec<usize> = (0..state.slots.len())
        .filter(|&i| inside.contains(&state.slots[i].entity_id))
        .collect();
    if holes.len() != inside.len() {
        return None;
    }
    let mut cursor = old.start.clone();
    let mut pieces = Vec::with_capacity(holes.len() + 1);
    let mut deferred = Vec::with_capacity(holes.len() + 1);
    for &i in &holes {
        let slot = &state.slots[i];
        let at = slot.prepaint_range.start.moved(base, &old.start);
        pieces.push(cursor.dispatch_index()..at.dispatch_index());
        deferred.push(cursor.deferred_index()..at.deferred_index());
        cursor = slot.prepaint_range.end.moved(base, &old.start);
        let ctx = window.peek_element_state::<ViewElementState, _>(&slot.global_id, |s| s.prepaint_ctx.is_some() && s.paint_ctx.is_some());
        if ctx != Some(true) {
            return None;
        }
    }
    pieces.push(cursor.dispatch_index()..old.end.dispatch_index());
    deferred.push(cursor.deferred_index()..old.end.deferred_index());
    deferred.into_iter().all(|d| window.deferred_within(d, &pieces)).then_some(holes)
}

/// Reuses `state`'s last drawing around `holes`, drawing those slots afresh where they were.
fn punch_prepaint(
    state: &mut ViewElementState,
    holes: Vec<usize>,
    base: &PrepaintStateIndex,
    old: Range<PrepaintStateIndex>,
    window: &mut Window,
    cx: &mut App,
) {
    let new_start = window.prepaint_index();
    let mut open = OpenReuse::default();
    let mut cursor = old.start.clone();
    let mut seg_new = new_start.clone();
    let mut drawn = Vec::with_capacity(holes.len());
    let mut next_hole = holes.iter().copied().peekable();
    for i in 0..state.slots.len() {
        let slot = &state.slots[i];
        let from = slot.prepaint_range.start.moved(base, &old.start);
        let to = slot.prepaint_range.end.moved(base, &old.start);
        if next_hole.peek() != Some(&i) {
            // Its own dispatch node opens just before it, in the same reused piece.
            let slot = &mut state.slots[i];
            slot.parent_node = slot.parent_node.map(|n| {
                n.moved(base.dispatch_index(), old.start.dispatch_index())
                    .moved(cursor.dispatch_index(), seg_new.dispatch_index())
            });
            slot.prepaint_range = from.moved(&cursor, &seg_new)..to.moved(&cursor, &seg_new);
            continue;
        }
        next_hole.next();
        window.reuse_prepaint_open(cursor.clone()..from.clone(), &mut open);
        let parent = slot.parent_node.map(|n| n.moved(base.dispatch_index(), old.start.dispatch_index()));
        window.next_frame.dispatch_tree.reuse_hole(parent, &mut open);
        let (entity_id, global_id, respawn) = (slot.entity_id, slot.global_id.clone(), slot.respawn.clone());
        let ctx = window
            .peek_element_state::<ViewElementState, _>(&global_id, |s| (s.prepaint_ctx.clone(), s.cache_key.bounds))
            .and_then(|(ctx, bounds)| Some((ctx?, bounds)));
        let Some((ctx, bounds)) = ctx else { unreachable!("checked by punch_holes") };
        window.punch_from = Some(from.clone()..to.clone());
        window.view_slots.push(Vec::new());
        let element = window.with_draw_ctx(&ctx, |window| {
            prepaint_view(entity_id, Some(&global_id), bounds, &mut None, window, cx, &mut |w, cx| respawn(w, cx), Some(&respawn))
        });
        let fresh = window.view_slots.pop().and_then(|mut v| v.pop());
        let painted = state.slots[i].paint_range.clone();
        if let Some(fresh) = fresh {
            state.slots[i] = fresh;
        }
        drawn.push((i, element, painted));
        cursor = to;
        seg_new = window.prepaint_index();
    }
    window.reuse_prepaint_open(cursor..old.end.clone(), &mut open);
    window.reuse_prepaint_close(&mut open);
    cx.entities.extend_accessed(state.accessed_entities.iter().copied());
    state.prepaint_range = new_start..window.prepaint_index();
    state.holes = drawn;
    state.punched = true;
}


#[inline(never)]
fn prepaint_component(
    name: &'static str,
    element: &mut Option<AnyElement>,
    window: &mut Window,
    cx: &mut App,
) -> Option<AnyElement> {
    window.with_id(ElementId::from(name), |window| {
        element.as_mut().unwrap().prepaint(window, cx);
    });
    Some(element.take().unwrap())
}

#[inline(never)]
fn paint_view(
    entity_id: EntityId,
    cached: bool,
    global_id: Option<&GlobalElementId>,
    element: &mut Option<AnyElement>,
    window: &mut Window,
    cx: &mut App,
) {
    let given = window.punch_paint_from.take();
    let caching = cached && !window.is_inspector_picking(cx);
    let ctx = caching.then(|| window.draw_ctx());
    let start = window.paint_index();
    window.with_rendered_view(entity_id, |window| {
        if caching {
            window.with_element_state::<ViewElementState, _>(
                global_id.unwrap(),
                |element_state, window| {
                    let mut element_state = element_state.unwrap();

                    let paint_start = window.paint_index();
                    let opacity = window.element_opacity();

                    // Prepaint can only predict the opacity paint applies: an ancestor's
                    // hover or drag-over style depends on this frame's hit test, which
                    // isn't known until prepaint finishes. The key records what was
                    // actually painted, and output replayed under a different opacity is
                    // redrawn on the next frame.
                    if let Some(element) = element {
                        let refreshing = mem::replace(&mut window.refreshing, true);
                        window.view_paints.push(Vec::new());
                        let ((), accessed_entities) =
                            cx.detect_accessed_entities(|cx| element.paint(window, cx));
                        let painted = window.view_paints.pop().unwrap_or_default();
                        element_state.slots_painted = place_paints(&mut element_state.slots, &painted);
                        element_state.accessed_entities.extend(accessed_entities);
                        window.refreshing = refreshing;
                        element_state.cache_key.opacity = opacity;
                    } else {
                        if element_state.punched {
                            punch_paint(&mut element_state, given, window, cx);
                        } else {
                            let base = element_state.paint_range.start.clone();
                            window.reuse_paint(given.unwrap_or_else(|| element_state.paint_range.clone()));
                            move_slot_paints(&mut element_state.slots, &base, &paint_start);
                        }
                        if element_state.cache_key.opacity.to_bits() != opacity.to_bits() {
                            window.request_animation_frame();
                        }
                    }

                    let paint_end = window.paint_index();
                    element_state.paint_range = paint_start..paint_end;
                    element_state.paint_ctx = ctx.map(Rc::new);

                    ((), element_state)
                },
            )
        } else {
            element.as_mut().unwrap().paint(window, cx);
        }
    });
    let end = window.paint_index();
    if caching && let Some(holder) = window.view_paints.last_mut() {
        holder.push((entity_id, start..end));
    }
}

/// Gives each slot the paint range it painted, in order; false if one went unpainted.
fn place_paints(slots: &mut [ViewSlot], painted: &[(EntityId, Range<PaintIndex>)]) -> bool {
    let mut from = 0;
    for slot in slots {
        let Some(at) = painted[from..].iter().position(|(id, _)| *id == slot.entity_id) else {
            return false;
        };
        slot.paint_range = painted[from + at].1.clone();
        from += at + 1;
    }
    true
}

/// Paints the reuse prepaint punched: the last paint around the slots drawn afresh.
fn punch_paint(state: &mut ViewElementState, given: Option<Range<PaintIndex>>, window: &mut Window, cx: &mut App) {
    let base = state.paint_range.start.clone();
    let old = given.unwrap_or_else(|| state.paint_range.clone());
    let mut cursor = old.start.clone();
    let mut seg_new = window.paint_index();
    let mut holes = mem::take(&mut state.holes).into_iter().peekable();
    for i in 0..state.slots.len() {
        let hole = holes.next_if(|(h, _, _)| *h == i);
        let painted = match &hole {
            Some((_, _, painted)) => painted.clone(),
            None => state.slots[i].paint_range.clone(),
        };
        let from = painted.start.moved(&base, &old.start);
        let to = painted.end.moved(&base, &old.start);
        let Some((_, mut element, _)) = hole else {
            state.slots[i].paint_range = from.moved(&cursor, &seg_new)..to.moved(&cursor, &seg_new);
            continue;
        };
        window.reuse_paint(cursor.clone()..from.clone());
        let (entity_id, global_id) = (state.slots[i].entity_id, state.slots[i].global_id.clone());
        let ctx = window.peek_element_state::<ViewElementState, _>(&global_id, |s| s.paint_ctx.clone()).flatten();
        let Some(ctx) = ctx else { unreachable!("checked by punch_holes") };
        window.punch_paint_from = Some(from..to.clone());
        let at = window.paint_index();
        window.view_paints.push(Vec::new());
        window.with_draw_ctx(&ctx, |window| paint_view(entity_id, true, Some(&global_id), &mut element, window, cx));
        window.view_paints.pop();
        state.slots[i].paint_range = at..window.paint_index();
        cursor = to;
        seg_new = window.paint_index();
    }
    window.reuse_paint(cursor..old.end);
}

#[inline(never)]
fn paint_component(
    name: &'static str,
    element: &mut Option<AnyElement>,
    window: &mut Window,
    cx: &mut App,
) {
    window.with_id(ElementId::Name(name.into()), |window| {
        element.as_mut().unwrap().paint(window, cx);
    });
}
