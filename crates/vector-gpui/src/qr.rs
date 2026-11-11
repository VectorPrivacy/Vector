//! QR codes drawn as `render/qr.js` draws them: a "liquid" SVG whose dark modules round off
//! where no neighbour continues the run, with rounded finder eyes and the Vector shield in a
//! knockout at the centre. ECC Q funds that knockout.

use std::fmt::Write;
use std::sync::Arc;

use gpui::RenderImage;
use qrcodegen::{Mask, QrCode, QrCodeEcc, QrSegment, Version};

const LOGO_W: f64 = 40.81;
const LOGO_H: f64 = 63.38;
const LOGO_PATHS: &str = concat!(
    r##"<path fill="#fff" d="M40.81 23.2a5 5 0 0 0-.55-2.29q.5-1.5.5-3.07V8.69A8.7 8.7 0 0 0 32.07 0h-.23a8.7 8.7 0 0 0-6.83 3.65 9.9 9.9 0 0 0-9.2 0A8.7 8.7 0 0 0 8.98 0h-.23A8.7 8.7 0 0 0 .06 8.69v9.15q0 1.57.5 3.07a5 5 0 0 0-.55 2.29v7.74q0 1.54.48 3.02a5 5 0 0 0-.48 2.15v7.74a9.8 9.8 0 0 0 3.18 7.22L13.8 60.8a9.77 9.77 0 0 0 13.24 0l10.61-9.73a9.8 9.8 0 0 0 3.18-7.22v-7.74q0-1.15-.48-2.15.48-1.47.48-3.02V23.2Z"/>"##,
    r##"<path fill="#000" d="m35.78 36.12-11.99 10.9a5 5 0 0 1-6.77 0L5.03 36.12l-.01-.01v7.74c0 1.34.56 2.62 1.55 3.52l10.61 9.73a4.76 4.76 0 0 0 6.46 0l10.61-9.73a4.8 4.8 0 0 0 1.55-3.52v-7.74z"/>"##,
    r##"<path fill="#000" d="m29.79 38.55 4.45-4.08a4.8 4.8 0 0 0 1.55-3.52v-7.74l-.01.01-11.99 10.9a5 5 0 0 1-6.77 0L5.03 23.22l-.01-.01v7.74c0 1.34.56 2.62 1.55 3.52l10.61 9.73a4.76 4.76 0 0 0 6.46 0l6.15-5.64"/>"##,
    r##"<path fill="#000" d="M31.97 5.02a3.73 3.73 0 0 0-3.58 3.76v6.28c0 1.05-.44 2.06-1.22 2.77l-3.42 3.13a5 5 0 0 1-3.35 1.32 5 5 0 0 1-3.35-1.32l-3.42-3.13a3.8 3.8 0 0 1-1.22-2.77V8.78c0-2.01-1.57-3.7-3.58-3.76a3.7 3.7 0 0 0-3.77 3.67v9.15c0 1.34.56 2.62 1.55 3.52l10.61 9.73c.87.8 2 1.24 3.17 1.25a4.7 4.7 0 0 0 3.17-1.25l10.61-9.73a4.8 4.8 0 0 0 1.55-3.52V8.69c0-2.06-1.7-3.73-3.77-3.67Z"/>"##,
    r##"<circle fill="#000" cx="20.41" cy="12.37" r="4.85"/>"##,
);

/// JS number formatting for the path data: integers bare, fractions as they come.
fn n(v: f64) -> String {
    if v.fract() == 0. { format!("{}", v as i64) } else { format!("{}", v) }
}

fn rounded_square(x: f64, y: f64, w: f64, r: f64) -> String {
    format!(
        "M{},{}h{}a{r},{r} 0 0 1 {r},{r}v{}a{r},{r} 0 0 1 {},{r}h{}a{r},{r} 0 0 1 {},{}v{}a{r},{r} 0 0 1 {r},{}z",
        n(x + r),
        n(y),
        n(w - 2. * r),
        n(w - 2. * r),
        n(-r),
        n(-(w - 2. * r)),
        n(-r),
        n(-r),
        n(-(w - 2. * r)),
        n(-r),
        r = n(r)
    )
}

/// The shield's silhouette, in logo units, grown by `m`.
fn shield_hit(lx: f64, ly: f64, m: f64) -> bool {
    if lx < -m || lx > LOGO_W + m || ly < -m || ly > LOGO_H + m {
        return false;
    }
    let shoulder = 9.;
    if ly < shoulder {
        if lx < shoulder {
            return (lx - shoulder).hypot(ly - shoulder) <= shoulder + m;
        }
        if lx > LOGO_W - shoulder {
            return (lx - (LOGO_W - shoulder)).hypot(ly - shoulder) <= shoulder + m;
        }
        return true;
    }
    let taper = 44.;
    if ly <= taper {
        return true;
    }
    let t = (LOGO_H - ly) / (LOGO_H - taper);
    (lx - LOGO_W / 2.).abs() <= (LOGO_W / 2.) * t.max(0.) + m
}

/// The symbol qrcode-generator makes for `text` (byte mode, ECC Q): the mask is the one its
/// own scoring picks, so both clients draw the same pattern.
fn encode(text: &str) -> Option<QrCode> {
    let segs = [QrSegment::make_bytes(text.as_bytes())];
    let make = |m: u8| QrCode::encode_segments_advanced(&segs, QrCodeEcc::Quartile, Version::MIN, Version::MAX, Some(Mask::new(m)), false).ok();
    let mut best: Option<(f64, QrCode)> = None;
    for m in 0..8 {
        let qr = make(m)?;
        let lost = lost_point(&qr);
        if best.as_ref().is_none_or(|(b, _)| *b > lost) {
            best = Some((lost, qr));
        }
    }
    best.map(|(_, qr)| qr)
}

/// qrcode-generator's `getLostPoint`, scored as it scores: format and version modules light.
fn lost_point(qr: &QrCode) -> f64 {
    let n = qr.size() as usize;
    let mut g: Vec<Vec<bool>> = (0..n).map(|r| (0..n).map(|c| qr.get_module(c as i32, r as i32)).collect()).collect();
    for i in 0..15 {
        let row = if i < 6 { i } else if i < 8 { i + 1 } else { n - 15 + i };
        g[row][8] = false;
        let col = if i < 8 { n - i - 1 } else if i < 9 { 7 } else { 15 - i - 1 };
        g[8][col] = false;
    }
    g[n - 8][8] = false;
    if qr.version().value() >= 7 {
        for i in 0..18 {
            g[i / 3][i % 3 + n - 11] = false;
            g[i % 3 + n - 11][i / 3] = false;
        }
    }
    let d = |r: usize, c: usize| g[r][c];
    let mut lost = 0f64;
    for row in 0..n {
        for col in 0..n {
            let mut same = 0;
            for dr in -1i32..=1 {
                for dc in -1i32..=1 {
                    let (r, c) = (row as i32 + dr, col as i32 + dc);
                    if (dr, dc) == (0, 0) || r < 0 || c < 0 || r >= n as i32 || c >= n as i32 {
                        continue;
                    }
                    if d(r as usize, c as usize) == d(row, col) {
                        same += 1;
                    }
                }
            }
            if same > 5 {
                lost += (3 + same - 5) as f64;
            }
        }
    }
    for row in 0..n - 1 {
        for col in 0..n - 1 {
            let count = [d(row, col), d(row + 1, col), d(row, col + 1), d(row + 1, col + 1)].iter().filter(|&&x| x).count();
            if count == 0 || count == 4 {
                lost += 3.;
            }
        }
    }
    let finder = [true, false, true, true, true, false, true];
    for row in 0..n {
        for col in 0..n.saturating_sub(6) {
            if (0..7).all(|k| d(row, col + k) == finder[k]) {
                lost += 40.;
            }
        }
    }
    for col in 0..n {
        for row in 0..n.saturating_sub(6) {
            if (0..7).all(|k| d(row + k, col) == finder[k]) {
                lost += 40.;
            }
        }
    }
    let dark = g.iter().flatten().filter(|&&x| x).count() as f64;
    lost += (100. * dark / n as f64 / n as f64 - 50.).abs() / 5. * 10.;
    lost
}

/// `buildCurvyQrSvg` for `text`, encoded as bytes at ECC Q.
pub fn svg(text: &str) -> Option<String> {
    let qr = encode(text)?;
    let count = qr.size();
    let s = 4.;
    let r = s * 0.375;
    let size = count as f64 * s;
    let finder = |row: i32, col: i32| (row < 7 && col < 7) || (row < 7 && col >= count - 7) || (row >= count - 7 && col < 7);
    let logo_h = size * 0.28;
    let k = logo_h / LOGO_H;
    let tx = (size - LOGO_W * k) / 2.;
    let ty = (size - logo_h) / 2.;
    let margin = (0.75 * s) / k;
    let holed = |row: i32, col: i32| {
        let (x, y) = (col as f64 * s, row as f64 * s);
        [(x, y), (x + s, y), (x, y + s), (x + s, y + s), (x + s / 2., y + s / 2.)].iter().any(|&(px, py)| shield_hit((px - tx) / k, (py - ty) / k, margin))
    };
    let dark = |row: i32, col: i32| row >= 0 && col >= 0 && row < count && col < count && qr.get_module(col, row) && !holed(row, col);

    let mut d = String::new();
    for row in 0..count {
        for col in 0..count {
            if finder(row, col) {
                continue;
            }
            let (x, y) = (col as f64 * s, row as f64 * s);
            let (top, bottom, left, right) = (dark(row - 1, col), dark(row + 1, col), dark(row, col - 1), dark(row, col + 1));
            if dark(row, col) {
                let tl = if !top && !left { r } else { 0. };
                let tr = if !top && !right { r } else { 0. };
                let br = if !bottom && !right { r } else { 0. };
                let bl = if !bottom && !left { r } else { 0. };
                let _ = write!(d, "M{},{}h{}", n(x + tl), n(y), n(s - tl - tr));
                if tr > 0. {
                    let _ = write!(d, "a{0},{0} 0 0 1 {0},{0}", n(tr));
                }
                let _ = write!(d, "v{}", n(s - tr - br));
                if br > 0. {
                    let _ = write!(d, "a{0},{0} 0 0 1 {1},{0}", n(br), n(-br));
                }
                let _ = write!(d, "h{}", n(-(s - br - bl)));
                if bl > 0. {
                    let _ = write!(d, "a{0},{0} 0 0 1 {1},{1}", n(bl), n(-bl));
                }
                let _ = write!(d, "v{}", n(-(s - bl - tl)));
                if tl > 0. {
                    let _ = write!(d, "a{0},{0} 0 0 1 {0},{1}", n(tl), n(-tl));
                }
                d.push('z');
            } else {
                // Fillets only at true inner corners: diagonal-only welds read as noise.
                if top && left && dark(row - 1, col - 1) {
                    let _ = write!(d, "M{},{}L{},{}A{r},{r} 0 0 0 {},{}z", n(x), n(y), n(x + r), n(y), n(x), n(y + r), r = n(r));
                }
                if top && right && dark(row - 1, col + 1) {
                    let _ = write!(d, "M{},{}L{},{}A{r},{r} 0 0 0 {},{}z", n(x + s), n(y), n(x + s), n(y + r), n(x + s - r), n(y), r = n(r));
                }
                if bottom && right && dark(row + 1, col + 1) {
                    let _ = write!(d, "M{},{}L{},{}A{r},{r} 0 0 0 {},{}z", n(x + s), n(y + s), n(x + s - r), n(y + s), n(x + s), n(y + s - r), r = n(r));
                }
                if bottom && left && dark(row + 1, col - 1) {
                    let _ = write!(d, "M{},{}L{},{}A{r},{r} 0 0 0 {},{}z", n(x), n(y + s), n(x), n(y + s - r), n(x + r), n(y + s), r = n(r));
                }
            }
        }
    }
    let mut eyes = String::new();
    let far = (count - 7) as f64 * s;
    for (fx, fy) in [(0., 0.), (far, 0.), (0., far)] {
        let _ = write!(
            eyes,
            r##"<path d="{}{}" fill="#000" fill-rule="evenodd"/><path d="{}" fill="#000"/>"##,
            rounded_square(fx, fy, 7. * s, 2.1 * s),
            rounded_square(fx + s, fy + s, 5. * s, 1.1 * s),
            rounded_square(fx + 2. * s, fy + 2. * s, 3. * s, 1.1 * s)
        );
    }
    Some(format!(
        r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 {sz} {sz}" preserveAspectRatio="xMinYMin meet"><path d="{d}" fill="#000"/>{eyes}<g transform="translate({tx:.2},{ty:.2}) scale({k:.4})">{LOGO_PATHS}</g></svg>"##,
        sz = n(size)
    ))
}

/// The QR on a transparent canvas `side` device pixels square.
pub fn image(text: &str, side: u32) -> Option<Arc<RenderImage>> {
    let svg = svg(text)?;
    let r = vector_core::svg::rasterize(svg.as_bytes(), side).ok()?;
    let mut buf = image::RgbaImage::from_raw(r.width, r.height, r.rgba)?;
    for px in buf.chunks_exact_mut(4) {
        px.swap(0, 2);
    }
    Some(Arc::new(RenderImage::new(vec![image::Frame::new(buf)])))
}
