//! What the desktop client's `util` gives the image code compiled in from it.

pub fn data_uri(mime: &str, bytes: &[u8]) -> String {
    let mut result = String::with_capacity(13 + mime.len() + bytes.len().div_ceil(3) * 4);
    result.push_str("data:");
    result.push_str(mime);
    result.push_str(";base64,");
    vector_core::simd::base64::STANDARD.encode_append(bytes, &mut result);
    result
}

pub use crate::simd::image::has_alpha_transparency;

/// A thumbhash of `img`, drawn from a 100 px thumbnail.
pub fn generate_thumbhash_from_image(img: &image::DynamicImage) -> Option<String> {
    const THUMBHASH_SIZE: u32 = 100;
    let (width, height) = (img.width(), img.height());
    let (thumb_w, thumb_h) = if width > height {
        (THUMBHASH_SIZE, (THUMBHASH_SIZE * height / width).max(1))
    } else {
        ((THUMBHASH_SIZE * width / height).max(1), THUMBHASH_SIZE)
    };
    let rgba = crate::shared::image::thumbnail(img, thumb_w, thumb_h).to_rgba8();
    let hash = fast_thumbhash::rgba_to_thumb_hash(
        rgba.width() as usize,
        rgba.height() as usize,
        rgba.as_raw(),
    );
    Some(fast_thumbhash::base91_encode(&hash))
}

/// `Intl.RelativeTimeFormat` (en, numeric: always) over `ttRelativeText`'s unit steps.
pub fn relative_time(unix: i64, now: i64) -> String {
    let diff = (unix - now) as f64;
    let abs = diff.abs();
    if abs < 0.5 {
        return "now".into();
    }
    let (n, unit) = if abs < 59.5 {
        (diff, "second")
    } else if abs < 2700. {
        (diff / 60., "minute")
    } else if abs < 79200. {
        (diff / 3600., "hour")
    } else if abs < 2246400. {
        (diff / 86400., "day")
    } else if abs < 28512000. {
        (diff / 2592000., "month")
    } else {
        (diff / 31536000., "year")
    };
    let n = n.round() as i64;
    let count = n.unsigned_abs();
    let unit = if count == 1 {
        unit.to_string()
    } else {
        format!("{unit}s")
    };
    if n < 0 || (n == 0 && diff < 0.) {
        format!("{count} {unit} ago")
    } else {
        format!("in {count} {unit}")
    }
}

#[cfg(test)]
mod relative_tests {
    #[test]
    fn steps_like_the_web_client() {
        assert_eq!(super::relative_time(0, 3600), "1 hour ago");
        assert_eq!(super::relative_time(86400 * 6, 0), "in 6 days");
        assert_eq!(super::relative_time(0, 30), "30 seconds ago");
    }
}
