//! The scene drops primitives a later opaque rectangle hides whole, and nothing else.

use gpui::{
    BorderStyle, Bounds, ContentMask, Corners, Edges, Hsla, Quad, Scene, ScaledPixels, Shadow, hsla, point, size,
};

fn rect(x: f32, y: f32, w: f32, h: f32) -> Bounds<ScaledPixels> {
    Bounds { origin: point(ScaledPixels(x), ScaledPixels(y)), size: size(ScaledPixels(w), ScaledPixels(h)) }
}

fn quad(bounds: Bounds<ScaledPixels>, color: Hsla, radius: f32) -> Quad {
    Quad {
        order: 0,
        border_style: BorderStyle::Solid,
        bounds,
        content_mask: ContentMask::new(rect(0., 0., 4000., 4000.)),
        background: color.into(),
        border_color: hsla(0., 0., 0.5, 1.),
        corner_radii: Corners::all(ScaledPixels(radius)),
        border_widths: Edges::all(ScaledPixels(0.)),
    }
}

fn finished(build: impl FnOnce(&mut Scene)) -> Scene {
    let mut scene = Scene::default();
    build(&mut scene);
    scene.finish();
    scene
}

const OPAQUE: Hsla = Hsla { h: 0., s: 0., l: 0.1, a: 1. };
const SEE_THROUGH: Hsla = Hsla { h: 0., s: 0., l: 0.1, a: 0.99 };

#[test]
fn stacked_backgrounds_leave_only_the_top_one() {
    let screen = rect(0., 0., 720., 1600.);
    let scene = finished(|s| {
        s.insert_primitive(quad(screen, OPAQUE, 0.));
        s.insert_primitive(quad(screen, OPAQUE, 0.));
        s.insert_primitive(quad(rect(10., 10., 100., 40.), SEE_THROUGH, 8.));
        s.insert_primitive(quad(screen, OPAQUE, 0.));
        s.insert_primitive(quad(rect(20., 20., 50., 50.), SEE_THROUGH, 4.));
    });
    assert_eq!(scene.quads.len(), 2);
    assert_eq!(scene.quads[1].bounds, rect(20., 20., 50., 50.));
}

#[test]
fn see_through_rounded_or_faded_covers_hide_nothing() {
    let screen = rect(0., 0., 720., 1600.);
    for top in [quad(screen, SEE_THROUGH, 0.), quad(screen, OPAQUE, 2.), {
        let mut q = quad(screen, OPAQUE, 0.);
        q.content_mask.fade.top_len = ScaledPixels(8.);
        q
    }, {
        let mut q = quad(screen, OPAQUE, 0.);
        q.content_mask.corner_radii = Corners::all(ScaledPixels(8.));
        q
    }] {
        let scene = finished(|s| {
            s.insert_primitive(quad(screen, OPAQUE, 0.));
            s.insert_primitive(top);
        });
        assert_eq!(scene.quads.len(), 2);
    }
}

#[test]
fn a_cover_counts_only_its_whole_pixels() {
    let cover = rect(10.5, 10.5, 300., 300.);
    let scene = finished(|s| {
        s.insert_primitive(quad(cover, OPAQUE, 0.));
        s.insert_primitive(quad(cover, OPAQUE, 0.));
        s.insert_primitive(quad(rect(11., 11., 299., 299.), OPAQUE, 0.));
        s.insert_primitive(quad(cover, OPAQUE, 0.));
    });
    // The first two keep only the half pixels round the cover's whole ones; the third lies inside.
    let whole = rect(11., 11., 299., 299.);
    let below: Vec<_> = scene.quads[..scene.quads.len() - 1].iter().map(|q| q.bounds).collect();
    assert_eq!(below.len(), 8);
    assert!(below.iter().all(|b| !b.intersects(&whole)), "{below:?}");
}

#[test]
fn a_shadow_counts_its_blur() {
    let card = rect(100., 100., 400., 400.);
    let shadow = |blur: f32| Shadow {
        order: 0,
        blur_radius: ScaledPixels(blur),
        bounds: card,
        corner_radii: Corners::all(ScaledPixels(0.)),
        content_mask: ContentMask::new(rect(0., 0., 1000., 1000.)),
        color: hsla(0., 0., 0., 0.5),
        element_bounds: card,
        element_corner_radii: Corners::all(ScaledPixels(0.)),
        inset: 0,
        pad: 0,
    };
    let scene = finished(|s| {
        s.insert_primitive(shadow(10.));
        s.insert_primitive(shadow(40.));
        s.insert_primitive(quad(rect(50., 50., 500., 500.), OPAQUE, 0.));
    });
    // Blur 10 reaches 30 past the card, inside the cover; blur 40 reaches 120, out of it.
    assert_eq!(scene.shadows.len(), 1);
    assert_eq!(scene.shadows[0].blur_radius, ScaledPixels(40.));
}

#[test]
fn what_lies_above_a_cover_stays() {
    let screen = rect(0., 0., 720., 1600.);
    let scene = finished(|s| {
        s.insert_primitive(quad(screen, OPAQUE, 0.));
        s.insert_primitive(quad(rect(0., 0., 100., 100.), OPAQUE, 0.));
        s.insert_primitive(quad(rect(0., 0., 50., 50.), SEE_THROUGH, 0.));
    });
    assert_eq!(scene.quads.len(), 3);
}

#[test]
fn a_backdrop_keeps_only_the_strip_left_showing() {
    let scene = finished(|s| {
        s.insert_primitive(quad(rect(0., 0., 720., 1600.), OPAQUE, 0.));
        s.insert_primitive(quad(rect(0., 0., 720., 1600.), SEE_THROUGH, 0.));
        s.insert_primitive(quad(rect(0., 48.4, 720., 1552.), OPAQUE, 0.));
    });
    assert_eq!(scene.quads.len(), 3);
    // Cut at the cover's first whole row, where its own coverage is total.
    assert_eq!(scene.quads[0].bounds, rect(0., 0., 720., 49.));
    // A see-through fill's hidden part shows nothing either.
    assert_eq!(scene.quads[1].bounds, rect(0., 0., 720., 49.));
}

#[test]
fn only_plain_fills_are_trimmed() {
    let below = |radius: f32, border: f32| {
        let mut q = quad(rect(0., 0., 720., 1600.), OPAQUE, radius);
        q.border_widths = Edges::all(ScaledPixels(border));
        q
    };
    for q in [below(4., 0.), below(0., 1.)] {
        let scene = finished(|s| {
            s.insert_primitive(q);
            s.insert_primitive(quad(rect(0., 48., 720., 1552.), OPAQUE, 0.));
        });
        assert_eq!(scene.quads[0].bounds, rect(0., 0., 720., 1600.));
    }
}

#[test]
fn a_cover_across_the_middle_leaves_two_strips() {
    let scene = finished(|s| {
        s.insert_primitive(quad(rect(0., 0., 1080., 1920.), OPAQUE, 0.));
        s.insert_primitive(quad(rect(0., 63., 1080., 1731.), OPAQUE, 0.));
        s.insert_primitive(quad(rect(0., 63., 1080., 1731.), SEE_THROUGH, 0.));
    });
    let bounds: Vec<_> = scene.quads.iter().map(|q| q.bounds).collect();
    assert_eq!(bounds, [rect(0., 0., 1080., 63.), rect(0., 1794., 1080., 126.), rect(0., 63., 1080., 1731.), rect(0., 63., 1080., 1731.)]);
}

#[test]
fn a_centred_cover_leaves_a_frame() {
    let scene = finished(|s| {
        s.insert_primitive(quad(rect(0., 0., 2400., 1200.), OPAQUE, 0.));
        s.insert_primitive(quad(rect(600., 100., 1200., 1000.), OPAQUE, 0.));
    });
    let bounds: Vec<_> = scene.quads.iter().map(|q| q.bounds).collect();
    assert_eq!(
        bounds,
        [
            rect(0., 0., 2400., 100.),
            rect(0., 1100., 2400., 100.),
            rect(0., 100., 600., 1000.),
            rect(1800., 100., 600., 1000.),
            rect(600., 100., 1200., 1000.),
        ]
    );
}
