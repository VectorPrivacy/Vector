use crate::{AnyElement, App, Bounds, Element, GlobalElementId, InspectorElementId, IntoElement, LayoutId, Pixels, Window};

/// Draws `child` with no input (CSS `pointer-events: none`) while `muted`: it shows as usual, but
/// its hitboxes hit nothing and its mouse listeners never run, so a press reaches what lies
/// beneath. A closing modal fading out takes no taps this way, and blocks none either.
pub fn pass_through(child: impl IntoElement, muted: bool) -> PassThrough {
    PassThrough { child: child.into_any_element(), muted }
}

/// See [`pass_through`].
pub struct PassThrough {
    child: AnyElement,
    muted: bool,
}

impl Element for PassThrough {
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
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, ()) {
        (self.child.request_layout(window, cx), ())
    }

    fn prepaint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        _: Bounds<Pixels>,
        _: &mut (),
        window: &mut Window,
        cx: &mut App,
    ) {
        let child = &mut self.child;
        window.with_input_muted(self.muted, |window| child.prepaint(window, cx));
    }

    fn paint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        _: Bounds<Pixels>,
        _: &mut (),
        _: &mut (),
        window: &mut Window,
        cx: &mut App,
    ) {
        let child = &mut self.child;
        window.with_input_muted(self.muted, |window| child.paint(window, cx));
    }
}
