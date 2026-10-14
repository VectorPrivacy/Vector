//! A crop the user drew, mapped onto the image it is for.
//!
//! The cropper measures on whatever copy the page showed, and a preview copy may be scaled
//! down. The rectangle carries the size it was measured at, and is scaled to the image
//! itself, upright, as the page showed it.

use serde::Deserialize;

/// A rectangle in the pixels of the copy the cropper showed, `sw` x `sh` when it says.
#[derive(Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
pub struct CropRect {
    pub x: u32,
    pub y: u32,
    pub w: u32,
    pub h: u32,
    #[serde(default)]
    pub sw: Option<u32>,
    #[serde(default)]
    pub sh: Option<u32>,
}

/// The image's size as shown: EXIF orientation applied, as a webview applies it.
pub fn upright_dims(bytes: &[u8]) -> Option<(u32, u32)> {
    use image::metadata::Orientation::*;
    use image::ImageDecoder;
    let mut decoder = image::ImageReader::new(std::io::Cursor::new(bytes)).with_guessed_format().ok()?.into_decoder().ok()?;
    let (w, h) = decoder.dimensions();
    Some(match decoder.orientation().ok() {
        Some(Rotate90 | Rotate270 | Rotate90FlipH | Rotate270FlipH) => (h, w),
        _ => (w, h),
    })
}

/// `crop` on an image `actual` in size: scaled from the copy it was measured on, kept inside
/// the image, and kept square when it was drawn square.
pub fn map_crop(crop: CropRect, actual: (u32, u32)) -> (u32, u32, u32, u32) {
    let (aw, ah) = actual;
    let (kx, ky) = match (crop.sw, crop.sh) {
        (Some(sw), Some(sh)) if sw > 0 && sh > 0 && (sw, sh) != actual => (aw as f64 / sw as f64, ah as f64 / sh as f64),
        _ => (1.0, 1.0),
    };
    let scale = |v: u32, k: f64| (v as f64 * k).round() as u32;
    let mut w = scale(crop.w, kx).clamp(1, aw.max(1));
    let mut h = scale(crop.h, ky).clamp(1, ah.max(1));
    if crop.w == crop.h {
        let side = w.min(h);
        (w, h) = (side, side);
    }
    let x = scale(crop.x, kx).min(aw.saturating_sub(w));
    let y = scale(crop.y, ky).min(ah.saturating_sub(h));
    (x, y, w, h)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rect(x: u32, y: u32, w: u32, h: u32, measured: Option<(u32, u32)>) -> CropRect {
        CropRect { x, y, w, h, sw: measured.map(|m| m.0), sh: measured.map(|m| m.1) }
    }

    #[test]
    fn a_crop_measured_on_a_scaled_preview_lands_on_the_same_part_of_the_original() {
        // A 4000 x 3000 photo previewed at 1024 x 768: the right half's square.
        assert_eq!(map_crop(rect(512, 0, 512, 512, Some((1024, 768))), (4000, 3000)), (2000, 0, 2000, 2000));
    }

    #[test]
    fn a_crop_measured_on_the_image_itself_is_kept_as_drawn() {
        assert_eq!(map_crop(rect(10, 20, 30, 40, None), (100, 100)), (10, 20, 30, 40));
        assert_eq!(map_crop(rect(10, 20, 30, 40, Some((100, 100))), (100, 100)), (10, 20, 30, 40));
    }

    #[test]
    fn rounding_never_leaves_the_image_or_unsquares_a_square() {
        let (x, y, w, h) = map_crop(rect(700, 700, 324, 324, Some((1024, 1024))), (1001, 1003));
        assert_eq!(w, h);
        assert!(x + w <= 1001 && y + h <= 1003, "{x} {y} {w} {h}");
    }
}

/// Crop an animated GIF without re-encoding it: each frame keeps its own palette, timing,
/// transparency and disposal, clipped to the rectangle. A frame wholly outside it lends its
/// delay to the one before. `None` when the file can't be cropped this way (not a GIF, a
/// rectangle outside the canvas, a first frame with nothing inside it); re-encode instead.
pub fn crop_gif(bytes: &[u8], x: u32, y: u32, w: u32, h: u32) -> Option<Vec<u8>> {
    if w == 0 || h == 0 || w > u16::MAX as u32 || h > u16::MAX as u32 {
        return None;
    }
    let mut opts = gif::DecodeOptions::new();
    opts.set_color_output(gif::ColorOutput::Indexed);
    let mut dec = opts.read_info(std::io::Cursor::new(bytes)).ok()?;
    if x.checked_add(w)? > dec.width() as u32 || y.checked_add(h)? > dec.height() as u32 {
        return None;
    }
    let global = dec.global_palette().map(<[u8]>::to_vec).unwrap_or_default();
    let repeat = dec.repeat();
    let mut out = Vec::new();
    {
        let mut enc = gif::Encoder::new(&mut out, w as u16, h as u16, &global).ok()?;
        enc.set_repeat(repeat).ok()?;
        let mut held: Option<gif::Frame<'static>> = None;
        while let Some(f) = dec.read_next_frame().ok()? {
            let (fl, ft, fw) = (f.left as u32, f.top as u32, f.width as u32);
            let (x0, y0) = (fl.max(x), ft.max(y));
            let (x1, y1) = ((fl + fw).min(x + w), (ft + f.height as u32).min(y + h));
            if x0 >= x1 || y0 >= y1 {
                held.as_mut()?.delay = held.as_ref()?.delay.saturating_add(f.delay);
                continue;
            }
            let nw = (x1 - x0) as usize;
            let mut buffer = Vec::with_capacity(nw * (y1 - y0) as usize);
            for row in y0..y1 {
                let start = ((row - ft) * fw + (x0 - fl)) as usize;
                buffer.extend_from_slice(f.buffer.get(start..start + nw)?);
            }
            let frame = gif::Frame {
                delay: f.delay,
                dispose: f.dispose,
                transparent: f.transparent,
                needs_user_input: false,
                left: (x0 - x) as u16,
                top: (y0 - y) as u16,
                width: nw as u16,
                height: (y1 - y0) as u16,
                interlaced: false,
                palette: f.palette.clone(),
                buffer: std::borrow::Cow::Owned(buffer),
            };
            if let Some(prev) = held.replace(frame) {
                enc.write_frame(&prev).ok()?;
            }
        }
        enc.write_frame(&held?).ok()?;
    }
    Some(out)
}

#[cfg(test)]
mod gif_tests {
    use super::crop_gif;

    fn decode(bytes: &[u8]) -> Vec<(image::RgbaImage, u32)> {
        use image::AnimationDecoder;
        let d = image::codecs::gif::GifDecoder::new(std::io::Cursor::new(bytes)).unwrap();
        d.into_frames()
            .map(|f| {
                let f = f.unwrap();
                let (n, dd) = f.delay().numer_denom_ms();
                (f.into_buffer(), n / dd.max(1))
            })
            .collect()
    }

    /// Three 6x4 frames: a full first frame, one change inside x=1..5, one only at x=0.
    fn sample() -> Vec<u8> {
        let mut out = Vec::new();
        {
            let palette = [0u8, 0, 0, 255, 0, 0, 0, 255, 0, 0, 0, 255];
            let mut enc = gif::Encoder::new(&mut out, 6, 4, &palette).unwrap();
            enc.set_repeat(gif::Repeat::Infinite).unwrap();
            let full: Vec<u8> = (0..24).map(|i| (i % 3) as u8).collect();
            let mut f = gif::Frame::from_indexed_pixels(6, 4, full, None);
            f.delay = 10;
            enc.write_frame(&f).unwrap();
            let mut inside = gif::Frame::from_indexed_pixels(2, 2, vec![1u8; 4], None);
            (inside.left, inside.top, inside.delay) = (2, 1, 20);
            enc.write_frame(&inside).unwrap();
            let mut outside = gif::Frame::from_indexed_pixels(1, 4, vec![2u8; 4], None);
            (outside.left, outside.delay) = (0, 30);
            enc.write_frame(&outside).unwrap();
        }
        out
    }

    #[test]
    fn a_gif_crop_matches_cropping_every_composited_frame() {
        let src = sample();
        let out = crop_gif(&src, 1, 0, 4, 4).expect("croppable");
        let (before, after) = (decode(&src), decode(&out));
        assert_eq!(after.len(), 2, "the frame outside the crop folds into the one before");
        assert_eq!(after[1].1, 500, "and lends it its delay (20 + 30 cs)");
        for (i, (img, _)) in after.iter().enumerate() {
            assert_eq!((img.width(), img.height()), (4, 4));
            let want = image::imageops::crop_imm(&before[i].0, 1, 0, 4, 4).to_image();
            assert_eq!(img.as_raw(), want.as_raw(), "frame {i}");
        }
    }

    #[test]
    fn what_cannot_be_cropped_losslessly_says_so() {
        let src = sample();
        assert!(crop_gif(&src, 3, 0, 4, 4).is_none(), "outside the canvas");
        assert!(crop_gif(b"not a gif", 0, 0, 1, 1).is_none());
        assert!(crop_gif(&src, 0, 0, 0, 0).is_none());
    }
}
