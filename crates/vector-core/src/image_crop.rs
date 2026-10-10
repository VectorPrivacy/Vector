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
