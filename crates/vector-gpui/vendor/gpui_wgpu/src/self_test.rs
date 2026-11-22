//! GPU self-test: times each kind of primitive on the device, one canned scene at a time, so a
//! log from one launch says where a slow frame's GPU time goes.

use super::*;
use gpui::{
    AtlasKey, BorderStyle, ContentMask, Corners, Edges, Hsla, ImageId, MonochromeSprite,
    PlatformAtlas, PolychromeSprite, Quad, RenderImageParams, RenderSvgParams, Shadow,
    TransformationMatrix, hsla, linear_color_stop, linear_gradient, point, px,
};
use std::borrow::Cow;
use std::time::{Duration, Instant};

const RUNS: usize = 5;

impl WgpuRenderer {
    /// One line per scene: its median and best GPU time. `current` is the frame on screen, timed
    /// offscreen and then to the surface, which together separate drawing from presenting.
    pub fn gpu_self_test(&mut self, current: &Scene) -> Vec<String> {
        let mut lines = Vec::new();
        let (width, height) = (self.surface_config.width, self.surface_config.height);
        let format = self.surface_config.format;
        let glyph_key = AtlasKey::Svg(RenderSvgParams {
            path: "gpu-self-test".into(),
            size: Size { width: DevicePixels(14), height: DevicePixels(18) },
        });
        let image_key = AtlasKey::Image(RenderImageParams { image_id: ImageId(usize::MAX), frame_index: 0 });
        let glyph = tile(&self.atlas, &glyph_key, 14, 18, 1);
        let image = tile(&self.atlas, &image_key, 64, 64, 4);
        let scenes = canned_scenes(width as f32, height as f32, glyph, image);

        if let RendererState::Ready { core, .. } = &mut self.state {
            let device = core.resources.device.clone();
            let texture = device.create_texture(&wgpu::TextureDescriptor {
                label: Some("self_test"),
                size: wgpu::Extent3d { width, height, depth_or_array_layers: 1 },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format,
                usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
                view_formats: &[],
            });
            let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
            let size = Size { width: DevicePixels(width as i32), height: DevicePixels(height as i32) };
            let scenes = scenes.iter().map(|(name, scene)| (*name, scene)).chain([("this frame, offscreen", current)]);
            for (name, scene) in scenes {
                let mut failed = None;
                let times = time(|| {
                    if let Err(error) = core.render_frame(scene, &view, None, size, false, wgpu::Color::BLACK) {
                        failed = Some(format!("{error:#}"));
                    }
                    wait(&device);
                });
                lines.push(match failed {
                    Some(error) => format!("{name}: failed: {error}"),
                    None => summary(name, times),
                });
            }
            texture.destroy();
        }

        lines.push(coverage(current, width as f32 * height as f32));
        let empty = finished(Scene::default());
        for (name, scene) in [("clear, to the screen", &empty), ("this frame, to the screen", current)] {
            let times = time(|| {
                self.draw(scene);
                if let Some(core) = self.core() {
                    wait(&core.resources.device);
                }
            });
            lines.push(summary(name, times));
        }

        self.atlas.remove(&glyph_key);
        self.atlas.remove(&image_key);
        self.needs_redraw = true;
        lines
    }
}

/// How many screens' worth of pixels the frame shades, by kind: overdraw on a GPU that pays per
/// pixel. Opaque quads are the part a smaller layer stack could drop.
fn coverage(scene: &Scene, screen: f32) -> String {
    fn area(bounds: Bounds<ScaledPixels>, mask: &ContentMask<ScaledPixels>) -> f32 {
        let b = bounds.intersect(&mask.bounds);
        b.size.width.0.max(0.0) * b.size.height.0.max(0.0)
    }
    let sum = |it: &mut dyn Iterator<Item = f32>| it.sum::<f32>() / screen;
    let opaque = sum(&mut scene.quads.iter().filter(|q| q.background.as_solid().is_some_and(|c| c.a >= 1.0)).map(|q| area(q.bounds, &q.content_mask)));
    let quads = sum(&mut scene.quads.iter().map(|q| area(q.bounds, &q.content_mask)));
    let shadows = sum(&mut scene.shadows.iter().map(|s| {
        let margin = ScaledPixels(3.0 * s.blur_radius.0);
        area(s.bounds.dilate(margin), &s.content_mask)
    }));
    let glyphs = sum(&mut scene.monochrome_sprites.iter().map(|g| area(g.bounds, &g.content_mask)));
    let images = sum(&mut scene.polychrome_sprites.iter().map(|i| area(i.bounds, &i.content_mask)));
    let paths = sum(&mut scene.paths.iter().map(|p| area(p.bounds, &p.content_mask)));
    // The biggest quads, so a layer that survives culling names itself.
    let mut big: Vec<&Quad> = scene.quads.iter().collect();
    big.sort_by(|a, b| area(b.bounds, &b.content_mask).total_cmp(&area(a.bounds, &a.content_mask)));
    let big: Vec<String> = big
        .iter()
        .take(4)
        .map(|q| {
            let b = q.bounds;
            let alpha = q.background.as_solid().map_or("gradient".to_string(), |c| format!("alpha {:.2}", c.a));
            let mask = &q.content_mask;
            format!(
                "#{} {:.0}x{:.0} at {:.1},{:.1} {alpha}, radius {:.0}, border {:.0}, mask radius {:.0}, mask fade {:.0}",
                q.order,
                b.size.width.0,
                b.size.height.0,
                b.origin.x.0,
                b.origin.y.0,
                q.corner_radii.top_left.0.max(q.corner_radii.bottom_right.0),
                q.border_widths.top.0.max(q.border_widths.left.0),
                mask.corner_radii.top_left.0.max(mask.corner_radii.bottom_right.0),
                mask.fade.top_len.0.max(mask.fade.bottom_len.0).max(mask.fade.left_len.0).max(mask.fade.right_len.0),
            )
        })
        .collect();
    format!(
        "this frame shades {:.2} screens: quads {quads:.2} ({opaque:.2} opaque), shadows {:.2}, glyphs {glyphs:.2}, images {images:.2}, paths {:.2}; biggest quads: {}",
        quads + shadows + glyphs + images + paths,
        shadows.abs(),
        paths.abs(),
        big.join("; ")
    )
}

fn wait(device: &wgpu::Device) {
    let _ = device.poll(wgpu::PollType::Wait { submission_index: None, timeout: None });
}

/// One warm-up run, then `RUNS` timed ones, sorted.
fn time(mut run: impl FnMut()) -> Vec<Duration> {
    run();
    let mut times: Vec<Duration> = (0..RUNS)
        .map(|_| {
            let started = Instant::now();
            run();
            started.elapsed()
        })
        .collect();
    times.sort();
    times
}

fn summary(name: &str, times: Vec<Duration>) -> String {
    let ms = |d: Duration| d.as_secs_f64() * 1000.0;
    format!("{name}: median {:.1} ms, best {:.1} ms", ms(times[RUNS / 2]), ms(times[0]))
}

fn tile(atlas: &WgpuAtlas, key: &AtlasKey, width: i32, height: i32, bpp: usize) -> Option<gpui::AtlasTile> {
    let size = Size { width: DevicePixels(width), height: DevicePixels(height) };
    let bytes = vec![0xC0u8; width as usize * height as usize * bpp];
    atlas
        .get_or_insert_with(key.clone(), &mut || Ok(Some((size, Cow::Borrowed(bytes.as_slice())))))
        .ok()
        .flatten()
}

fn finished(mut scene: Scene) -> Scene {
    scene.finish();
    scene
}

fn rect(x: f32, y: f32, width: f32, height: f32) -> Bounds<ScaledPixels> {
    Bounds {
        origin: Point { x: ScaledPixels(x), y: ScaledPixels(y) },
        size: Size { width: ScaledPixels(width), height: ScaledPixels(height) },
    }
}

fn quad(bounds: Bounds<ScaledPixels>, background: Background, radius: f32, border: f32) -> Quad {
    Quad {
        order: 0,
        border_style: BorderStyle::Solid,
        bounds,
        content_mask: ContentMask::new(bounds),
        background,
        border_color: hsla(0.0, 0.0, 0.5, 1.0),
        corner_radii: Corners::all(ScaledPixels(radius)),
        border_widths: Edges::all(ScaledPixels(border)),
    }
}

fn gray(lightness: f32, alpha: f32) -> Hsla {
    hsla(0.0, 0.0, lightness, alpha)
}

fn canned_scenes(
    width: f32,
    height: f32,
    glyph: Option<gpui::AtlasTile>,
    image: Option<gpui::AtlasTile>,
) -> Vec<(&'static str, Scene)> {
    let screen = rect(0.0, 0.0, width, height);
    let mut scenes = Vec::new();
    let mut add = |name, build: &dyn Fn(&mut Scene)| {
        let mut scene = Scene::default();
        build(&mut scene);
        scenes.push((name, finished(scene)));
    };

    add("clear", &|_| {});
    add("1 full-screen rect", &|s| s.insert_primitive(quad(screen, gray(0.1, 1.0).into(), 0.0, 0.0)));
    add("8 stacked translucent full-screen rects", &|s| {
        for i in 0..8 {
            s.insert_primitive(quad(screen, gray(0.1 * i as f32, 0.3).into(), 0.0, 0.0));
        }
    });
    add("full-screen rounded bordered card", &|s| {
        s.insert_primitive(quad(screen, gray(0.15, 1.0).into(), 32.0, 2.0))
    });
    add("full-screen gradient", &|s| {
        let gradient = linear_gradient(180.0, linear_color_stop(gray(0.1, 1.0), 0.0), linear_color_stop(gray(0.3, 1.0), 1.0));
        s.insert_primitive(quad(screen, gradient, 0.0, 0.0))
    });
    add("40 rounded buttons", &|s| {
        for i in 0..40 {
            let (col, row) = ((i % 4) as f32, (i / 4) as f32);
            let bounds = rect(16.0 + col * (width / 4.0), 16.0 + row * 120.0, width / 4.0 - 32.0, 96.0);
            s.insert_primitive(quad(bounds, gray(0.2, 1.0).into(), 16.0, 1.0));
        }
    });
    let shadow = |inset: u32, blur: f32| {
        let bounds = rect(width * 0.1, height * 0.3, width * 0.8, height * 0.4);
        Shadow {
            order: 0,
            blur_radius: ScaledPixels(blur),
            bounds,
            corner_radii: Corners::all(ScaledPixels(24.0)),
            content_mask: ContentMask::new(screen),
            color: gray(0.0, 0.5),
            element_bounds: bounds,
            element_corner_radii: Corners::all(ScaledPixels(24.0)),
            inset,
            pad: 0,
        }
    };
    add("card shadow, blur 24", &|s| s.insert_primitive(shadow(0, 24.0)));
    add("card shadow outside the card, blur 24", &|s| s.insert_primitive(shadow(2, 24.0)));
    add("card shadow, blur 8", &|s| s.insert_primitive(shadow(0, 8.0)));
    if let Some(tile) = glyph {
        let glyph = |bounds| MonochromeSprite {
            order: 0,
            pad: 0,
            tile: tile.clone(),
            bounds,
            content_mask: ContentMask::new(screen),
            color: gray(0.9, 1.0),
            transformation: TransformationMatrix::unit(),
        };
        let glyph = &glyph;
        let grid = |side: f32| {
            move |s: &mut Scene| {
                for i in 0..700 {
                    let (col, row) = ((i % 35) as f32, (i / 35) as f32);
                    s.insert_primitive(glyph(rect(8.0 + col * 20.0, 40.0 + row * 40.0, side * 14.0, side * 18.0)));
                }
            }
        };
        add("700 glyphs", &grid(1.0));
        // Per glyph and per pixel apart: a frame of text pays for both.
        add("700 glyphs a pixel across", &grid(1.0 / 14.0));
        add("1 glyph, full-screen", &|s| s.insert_primitive(glyph(screen)));
    }
    if let Some(tile) = image {
        let sprite = |bounds, radius| PolychromeSprite {
            order: 0,
            pad: 0,
            tile: tile.clone(),
            grayscale: false.into(),
            opacity: 1.0,
            bounds,
            content_mask: ContentMask::new(screen),
            corner_radii: Corners::all(ScaledPixels(radius)),
        };
        add("full-screen image", &|s| s.insert_primitive(sprite(screen, 0.0)));
        add("20 round avatars", &|s| {
            for i in 0..20 {
                s.insert_primitive(sprite(rect(16.0, 16.0 + i as f32 * 80.0, 72.0, 72.0), 36.0));
            }
        });
    }
    add("filled path, a third of the screen", &|s| {
        let (cx, cy, r) = (width / 2.0, height / 2.0, width * 0.4);
        let mut path = gpui::Path::new(point(px(cx + r), px(cy)));
        for step in 1..=32 {
            let a = step as f32 / 32.0 * std::f32::consts::TAU;
            path.line_to(point(px(cx + r * a.cos()), px(cy + r * a.sin())));
        }
        let mut path = path.scale(1.0);
        path.content_mask = ContentMask::new(screen);
        path.color = gray(0.5, 1.0).into();
        s.insert_primitive(path);
    });
    scenes
}
