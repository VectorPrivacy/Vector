//! The image crate's separable resize, the same arithmetic in the same order, so the output is
//! bit-identical. Each output row is filtered down the columns into one float row, then across:
//! no full-size float copy of the picture is ever held.

use image::imageops::FilterType;
use image::{DynamicImage, ImageBuffer, Pixel};

fn kernel(filter: FilterType) -> Option<(fn(f32) -> f32, f32)> {
    match filter {
        FilterType::Triangle => Some((triangle, 1.)),
        FilterType::CatmullRom => Some((catmull_rom, 2.)),
        _ => None,
    }
}

fn triangle(x: f32) -> f32 {
    if x.abs() < 1.0 { 1.0 - x.abs() } else { 0.0 }
}

fn catmull_rom(x: f32) -> f32 {
    let (b, c) = (0.0f32, 0.5f32);
    let a = x.abs();
    let k = if a < 1.0 {
        (12.0 - 9.0 * b - 6.0 * c) * a.powi(3) + (-18.0 + 12.0 * b + 6.0 * c) * a.powi(2) + (6.0 - 2.0 * b)
    } else if a < 2.0 {
        (-b - 6.0 * c) * a.powi(3) + (6.0 * b + 30.0 * c) * a.powi(2) + (-12.0 * b - 48.0 * c) * a + (8.0 * b + 24.0 * c)
    } else {
        0.0
    };
    k / 6.0
}

/// Per output index, the first source index and how many follow; weights flattened alongside.
struct Taps {
    spans: Vec<(usize, usize, usize)>,
    weights: Vec<f32>,
}

impl Taps {
    fn new(len: u32, new_len: u32, kernel: fn(f32) -> f32, support: f32) -> Taps {
        let ratio = len as f32 / new_len as f32;
        let sratio = if ratio < 1.0 { 1.0 } else { ratio };
        let src_support = support * sratio;
        let mut spans = Vec::with_capacity(new_len as usize);
        let mut weights = Vec::new();
        for out in 0..new_len {
            let input = (out as f32 + 0.5) * ratio;
            let left = ((input - src_support).floor() as i64).clamp(0, len as i64 - 1);
            let right = ((input + src_support).ceil() as i64).clamp(left + 1, len as i64);
            let input = input - 0.5;
            let at = weights.len();
            let mut sum = 0.0;
            for i in left..right {
                let w = kernel((i as f32 - input) / sratio);
                weights.push(w);
                sum += w;
            }
            for w in &mut weights[at..] {
                *w /= sum;
            }
            spans.push((left as usize, (right - left) as usize, at));
        }
        Taps { spans, weights }
    }

    fn iter(&self) -> impl Iterator<Item = (usize, &[f32])> {
        self.spans.iter().map(|&(left, n, at)| (left, &self.weights[at..at + n]))
    }
}

type TapsKey = (u32, u32, usize, u32);

thread_local! {
    /// The last few filters and the float row: an animation's frames all resize alike.
    static TAPS: std::cell::RefCell<Vec<(TapsKey, std::rc::Rc<Taps>)>> = const { std::cell::RefCell::new(Vec::new()) };
    static COLUMN: std::cell::Cell<Vec<f32>> = const { std::cell::Cell::new(Vec::new()) };
}

fn taps(len: u32, new_len: u32, kernel: fn(f32) -> f32, support: f32) -> std::rc::Rc<Taps> {
    let key = (len, new_len, kernel as usize, support.to_bits());
    TAPS.with_borrow_mut(|all| {
        if let Some(i) = all.iter().position(|(k, _)| *k == key) {
            let hit = all.remove(i);
            all.push(hit);
            return all.last().expect("just pushed").1.clone();
        }
        let taps = std::rc::Rc::new(Taps::new(len, new_len, kernel, support));
        // Only small filters are kept: each thread holds its own.
        if taps.weights.len() <= 8192 && new_len <= 1024 {
            if all.len() == 4 {
                all.remove(0);
            }
            all.push((key, taps.clone()));
        }
        taps
    })
}

/// One output row: the source rows its span reaches folded down the columns, then filtered across.
fn row_out<'a, const C: usize>(column: &mut [f32], lines: impl Iterator<Item = (&'a [u8], f32)>, across: &Taps, out: &mut [u8]) {
    column.fill(0.0);
    for (line, wt) in lines {
        fold(column, line, wt);
    }
    across_row::<C>(column, across, out);
}

fn fold(column: &mut [f32], line: &[u8], wt: f32) {
    for (acc, &v) in column.iter_mut().zip(line) {
        *acc += v as f32 * wt;
    }
}

fn across_row<const C: usize>(column: &[f32], across: &Taps, out: &mut [u8]) {
    for ((left, ws), px) in across.iter().zip(out.chunks_exact_mut(C)) {
        let mut t = [0f32; C];
        for (i, &wt) in ws.iter().enumerate() {
            let p = &column[(left + i) * C..][..C];
            for c in 0..C {
                t[c] += p[c] * wt;
            }
        }
        for c in 0..C {
            px[c] = to_u8(t[c]);
        }
    }
}

/// `v.clamp(0.0, 255.0).round() as u8` without `round`, a library call on SSE2.
#[inline(always)]
fn to_u8(v: f32) -> u8 {
    let v = v.clamp(0.0, 255.0);
    let whole = v as u32;
    (whole + u32::from(v - whole as f32 >= 0.5)) as u8
}

fn run<const C: usize>(src: &[u8], w: u32, h: u32, nw: u32, nh: u32, kernel: fn(f32) -> f32, support: f32) -> Vec<u8> {
    let (w, nwu) = (w as usize, nw as usize);
    let down = taps(h, nh, kernel, support);
    let across = taps(w as u32, nw, kernel, support);
    let mut out = vec![0u8; nwu * nh as usize * C];
    let mut column = COLUMN.take();
    column.clear();
    column.resize(w * C, 0.);
    for ((top, ws), row) in down.iter().zip(out.chunks_exact_mut(nwu * C)) {
        let lines = ws.iter().enumerate().map(|(j, &wt)| (&src[(top + j) * w * C..][..w * C], wt));
        row_out::<C>(&mut column, lines, &across, row);
    }
    // A wide row is let go rather than held for the thread's life.
    if column.capacity() <= 1 << 14 {
        COLUMN.set(column);
    }
    out
}

/// [`resample`] fed its source a row at a time, top to bottom, so a decoder scales as it goes:
/// each row folds straight into the output rows it reaches, and nothing else of the source is
/// held. Bit-identical to [`resample`]: an output row still takes its source rows in order.
pub struct Rows<const C: usize> {
    n: usize,
    down: std::rc::Rc<Taps>,
    across: std::rc::Rc<Taps>,
    /// A float row per output row still taking source rows, by output row modulo `open`.
    acc: Vec<f32>,
    open: usize,
    started: usize,
    next: usize,
    out: Vec<u8>,
}

impl<const C: usize> Rows<C> {
    /// None for a filter [`resample`] hands to the image crate, or an empty side.
    pub fn new(w: u32, h: u32, nw: u32, nh: u32, filter: FilterType) -> Option<Self> {
        let (kernel, support) = kernel(filter)?;
        if w == 0 || h == 0 || nw == 0 || nh == 0 {
            return None;
        }
        let down = taps(h, nh, kernel, support);
        let across = taps(w, nw, kernel, support);
        // The most output rows one source row reaches.
        let (mut open, mut first) = (1, 0);
        for (r, &(top, _, _)) in down.spans.iter().enumerate() {
            while down.spans[first].0 + down.spans[first].1 <= top {
                first += 1;
            }
            open = open.max(r + 1 - first);
        }
        let n = w as usize * C;
        Some(Self { n, acc: vec![0.; open * n], open, started: 0, next: 0, out: vec![0; nw as usize * C], down, across })
    }

    /// Source row `y`, which follows row `y - 1`: each output row it completes goes to `emit`.
    pub fn push(&mut self, y: usize, row: &[u8], mut emit: impl FnMut(usize, &[u8])) {
        let n = self.n;
        while self.down.spans.get(self.started).is_some_and(|s| s.0 <= y) {
            self.acc[(self.started % self.open) * n..][..n].fill(0.0);
            self.started += 1;
        }
        let row = &row[..n];
        for r in self.next..self.started {
            let (top, len, at) = self.down.spans[r];
            if y < top + len {
                fold(&mut self.acc[(r % self.open) * n..][..n], row, self.down.weights[at + y - top]);
            }
        }
        while self.next < self.started {
            let (top, len, _) = self.down.spans[self.next];
            if top + len > y + 1 {
                break;
            }
            across_row::<C>(&self.acc[(self.next % self.open) * n..][..n], &self.across, &mut self.out);
            emit(self.next, &self.out);
            self.next += 1;
        }
    }
}

/// `image::imageops::resize` for 8-bit buffers, without its full-size float copy; other
/// filters go to the image crate.
pub fn resample<P: Pixel<Subpixel = u8> + 'static>(
    img: &ImageBuffer<P, Vec<u8>>,
    nw: u32,
    nh: u32,
    filter: FilterType,
) -> ImageBuffer<P, Vec<u8>> {
    let (w, h) = img.dimensions();
    let Some((k, support)) = kernel(filter).filter(|_| w > 0 && h > 0 && nw > 0 && nh > 0 && (w, h) != (nw, nh))
    else {
        return image::imageops::resize(img, nw, nh, filter);
    };
    let raw = match P::CHANNEL_COUNT {
        1 => run::<1>(img.as_raw(), w, h, nw, nh, k, support),
        2 => run::<2>(img.as_raw(), w, h, nw, nh, k, support),
        3 => run::<3>(img.as_raw(), w, h, nw, nh, k, support),
        4 => run::<4>(img.as_raw(), w, h, nw, nh, k, support),
        _ => return image::imageops::resize(img, nw, nh, filter),
    };
    ImageBuffer::from_raw(nw, nh, raw).expect("sized to fit")
}

/// `DynamicImage::resize_exact` through [`resample`] for 8-bit images.
pub fn resample_exact(img: &DynamicImage, nw: u32, nh: u32, filter: FilterType) -> DynamicImage {
    match img {
        DynamicImage::ImageLuma8(b) => DynamicImage::ImageLuma8(resample(b, nw, nh, filter)),
        DynamicImage::ImageLumaA8(b) => DynamicImage::ImageLumaA8(resample(b, nw, nh, filter)),
        DynamicImage::ImageRgb8(b) => DynamicImage::ImageRgb8(resample(b, nw, nh, filter)),
        DynamicImage::ImageRgba8(b) => DynamicImage::ImageRgba8(resample(b, nw, nh, filter)),
        _ => img.resize_exact(nw, nh, filter),
    }
}

/// `DynamicImage::resize`: the largest size within `nw` x `nh` that keeps the aspect ratio.
pub fn resample_within(img: &DynamicImage, nw: u32, nh: u32, filter: FilterType) -> DynamicImage {
    let (w, h) = (img.width(), img.height());
    if (nw, nh) == (w, h) {
        return img.clone();
    }
    let r = (nw as f64 / w as f64).min(nh as f64 / h as f64);
    let fit = |len: u32| ((len as f64 * r).round() as u64).clamp(1, u32::MAX as u64) as u32;
    resample_exact(img, fit(w), fit(h), filter)
}
