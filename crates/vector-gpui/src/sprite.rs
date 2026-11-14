//! Procedural loops as sprite strips: each steps its tile in the presented scene, so a pulse
//! or a beat costs no re-render of the window.

use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::Arc;

use gpui::RenderImage;

/// One strip per `name` and device scale, drawn once by `paint(t, buf, k)` for each frame
/// time `t` in `times`; a frame lasts until the next one's time, the last until `period`.
pub fn strip(
    name: &'static str,
    scale: f32,
    side: u32,
    times: &[f32],
    period: f32,
    paint: impl Fn(f32, &mut image::RgbaImage, f32),
) -> Arc<RenderImage> {
    thread_local!(static STRIPS: RefCell<HashMap<(&'static str, u32), Arc<RenderImage>>> = RefCell::default());
    let k = scale.ceil().max(1.) as u32;
    STRIPS.with(|s| {
        s.borrow_mut()
            .entry((name, k))
            .or_insert_with(|| {
                let frames = times.iter().enumerate().map(|(i, &t)| {
                    let mut buf = image::RgbaImage::new(side * k, side * k);
                    paint(t, &mut buf, k as f32);
                    let end = times.get(i + 1).copied().unwrap_or(period);
                    let ms = ((end - t) * 1000.).round().max(1.) as u32;
                    image::Frame::from_parts(to_bgra(buf), 0, 0, image::Delay::from_numer_denom_ms(ms, 1))
                });
                Arc::new(RenderImage::new(frames.collect::<Vec<_>>()))
            })
            .clone()
    })
}

/// Evenly spaced frame times over `span` seconds at `fps`.
pub fn times(span: f32, fps: f32) -> Vec<f32> {
    let n = (span * fps).round().max(1.) as usize;
    (0..n).map(|i| i as f32 * span / n as f32).collect()
}

/// A filled anti-aliased disc of straight-alpha `rgb` at `alpha`, in device pixels.
pub fn disc(buf: &mut image::RgbaImage, cx: f32, cy: f32, r: f32, rgb: u32, alpha: f32) {
    if alpha <= 0. || r <= 0. {
        return;
    }
    let [_, red, green, blue] = rgb.to_be_bytes();
    let (x0, x1) = ((cx - r - 1.).max(0.) as u32, ((cx + r + 1.) as u32).min(buf.width()));
    let (y0, y1) = ((cy - r - 1.).max(0.) as u32, ((cy + r + 1.) as u32).min(buf.height()));
    for y in y0..y1 {
        for x in x0..x1 {
            let d = ((x as f32 + 0.5 - cx).powi(2) + (y as f32 + 0.5 - cy).powi(2)).sqrt();
            let a = (r + 0.5 - d).clamp(0., 1.) * alpha;
            if a <= 0. {
                continue;
            }
            // Source over, straight alpha.
            let under = buf.get_pixel(x, y).0;
            let ua = under[3] as f32 / 255.;
            let out = a + ua * (1. - a);
            let mix = |s: u8, u: u8| ((s as f32 * a + u as f32 * ua * (1. - a)) / out).round() as u8;
            buf.put_pixel(x, y, image::Rgba([mix(red, under[0]), mix(green, under[1]), mix(blue, under[2]), (out * 255.).round() as u8]));
        }
    }
}

fn to_bgra(mut buf: image::RgbaImage) -> image::RgbaImage {
    for px in buf.chunks_exact_mut(4) {
        px.swap(0, 2);
    }
    buf
}
