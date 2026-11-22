//! Cached panes: a slice of `VectorApp` drawn as a view of its own, so a change local to one
//! pane (a hover, a caret, a wake it asked for) leaves the others' layout and paint in place.
//! Any notify of `VectorApp` itself still redraws every pane.

use std::rc::Rc;

use gpui::prelude::*;
use gpui::{AnyElement, AnyView, Context, Entity, StyleRefinement, WeakEntity, Window, div};

use crate::ui::VectorApp;

type Build = Rc<dyn Fn(&mut VectorApp, &mut Window, &mut Context<VectorApp>) -> AnyElement>;

pub struct Pane {
    app: WeakEntity<VectorApp>,
    build: Option<Build>,
}

impl Render for Pane {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let Some(build) = self.build.clone() else {
            return div().into_any_element();
        };
        let el = self
            .app
            .update(cx, |app, cx| build(app, window, cx))
            .unwrap_or_else(|_| div().into_any_element());
        if crate::kit::easing() {
            window.request_animation_frame();
        }
        el
    }
}

/// The panes the main screen splits into.
pub struct Panes {
    pub side: Entity<Pane>,
    pub rail: Entity<Pane>,
    pub chat: Entity<Pane>,
    pub members: Entity<Pane>,
    pub timeline: Entity<Pane>,
    /// What floats over the timeline and follows its scroll: the scrollbar, the way back, typing.
    pub overlay: Entity<Pane>,
    /// Empty: notifying it redraws only what the root draws itself (the call panel, the rail).
    pub chrome: Entity<Pane>,
    /// Settings, wide or slim: its entrances and hovers redraw it alone.
    pub settings: Entity<Pane>,
    /// The start menu over everything.
    pub start: Entity<Pane>,
}

impl Panes {
    pub fn new(cx: &mut Context<VectorApp>) -> Self {
        let app = cx.entity().downgrade();
        let mut pane = || cx.new(|_| Pane { app: app.clone(), build: None });
        Self {
            side: pane(),
            rail: pane(),
            chat: pane(),
            members: pane(),
            timeline: pane(),
            overlay: pane(),
            chrome: pane(),
            settings: pane(),
            start: pane(),
        }
    }
}

/// `pane` laid out at `style`, drawing `build` whenever it is redrawn.
pub fn cached(
    pane: &Entity<Pane>,
    style: StyleRefinement,
    build: impl Fn(&mut VectorApp, &mut Window, &mut Context<VectorApp>) -> AnyElement + 'static,
    cx: &mut Context<VectorApp>,
) -> AnyElement {
    pane.update(cx, |p, _| p.build = Some(Rc::new(build)));
    AnyView::from(pane.clone()).cached(style).into_any_element()
}

/// Redraw `pane` alone; off screen, GPUI falls back to redrawing the window.
pub fn notify(pane: &Entity<Pane>, cx: &mut gpui::App) {
    pane.update(cx, |_, cx| cx.notify());
}

/// Redraw `pane` for a change its keyed rows carry in their keys: the rest replay.
pub fn notify_keyed(pane: &Entity<Pane>, cx: &mut gpui::App) {
    cx.notify_motion(pane.entity_id());
}
