use crate::{
    AnyElement, App, Bounds, Element, GlobalElementId, InspectorElementId, IntoElement, LayoutId,
    Pixels, Point, Window, point,
};

/// Draws `child` scaled about a point of its box, as CSS `transform: scale()` does: layout and
/// hit testing keep the unscaled box.
pub fn zoom(child: impl IntoElement) -> Zoom {
    Zoom { child: child.into_any_element(), scale: 1., origin: point(0.5, 0.5), opacity: None }
}

/// See [`zoom`].
pub struct Zoom {
    child: AnyElement,
    scale: f32,
    origin: Point<f32>,
    opacity: Option<f32>,
}

impl Zoom {
    /// The scale, 1 for none.
    pub fn scale(mut self, scale: f32) -> Self {
        self.scale = scale;
        self
    }

    /// The fixed point, as fractions of the box (`transform-origin`); the centre by default.
    pub fn origin(mut self, x: f32, y: f32) -> Self {
        self.origin = point(x, y);
        self
    }

    /// Fades the whole subtree, as `opacity` would on a wrapping div.
    pub fn opacity(mut self, opacity: f32) -> Self {
        self.opacity = Some(opacity);
        self
    }

    fn pivot(&self, bounds: Bounds<Pixels>) -> Point<Pixels> {
        point(
            bounds.origin.x + bounds.size.width * self.origin.x,
            bounds.origin.y + bounds.size.height * self.origin.y,
        )
    }
}

impl Element for Zoom {
    type RequestLayoutState = ();
    type PrepaintState = ();

    fn id(&self) -> Option<crate::ElementId> {
        None
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
        (self.child.request_layout(window, cx), ())
    }

    fn prepaint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _request_layout: &mut (),
        window: &mut Window,
        cx: &mut App,
    ) {
        let pivot = self.pivot(bounds);
        let opacity = self.opacity;
        window.with_element_zoom(pivot, self.scale, |window| {
            window.with_element_opacity(opacity, |window| self.child.prepaint(window, cx))
        });
    }

    fn paint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _request_layout: &mut (),
        _prepaint: &mut (),
        window: &mut Window,
        cx: &mut App,
    ) {
        let pivot = self.pivot(bounds);
        let opacity = self.opacity;
        window.with_element_zoom(pivot, self.scale, |window| {
            window.with_element_opacity(opacity, |window| self.child.paint(window, cx))
        });
    }
}
