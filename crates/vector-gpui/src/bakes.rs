//! Pictures kept on disk at the size they're drawn, as raw BGRA a first frame can read whole:
//! sealed like the cache they come from, so no plain pixels sit in the app's folders.

use std::path::{Path, PathBuf};

/// FNV-1a of a path: the stem every bake of one source starts with.
pub fn source_hash(path: &Path) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in path.as_os_str().as_encoded_bytes() {
        h = (h ^ u64::from(*b)).wrapping_mul(0x0100_0000_01b3);
    }
    h
}

/// `dir`'s bake for `tag` at `w`×`h`.
pub fn file(dir: &Path, tag: &str, (w, h): (u32, u32)) -> PathBuf {
    dir.join(format!("{tag}{w}x{h}.bgra"))
}

/// A bake as written: its size, then its pixels. One left unsealed is dropped.
pub fn load(file: &Path, want: Option<(u32, u32)>) -> Option<image::RgbaImage> {
    if !vector_core::vcache::is_sealed(file) {
        let _ = std::fs::remove_file(file);
        return None;
    }
    let mut bytes = vector_core::vcache::read(file).ok()?;
    let (head, px) = bytes.split_at_checked(8)?;
    let size = (u32::from_le_bytes(head[..4].try_into().ok()?), u32::from_le_bytes(head[4..].try_into().ok()?));
    let (w, h) = size;
    if want.is_some_and(|want| want != size) || px.len() as u64 != u64::from(w) * u64::from(h) * 4 {
        return None;
    }
    bytes.drain(..8);
    image::RgbaImage::from_raw(w, h, std::mem::take(&mut *bytes))
}

/// `tag`'s newest bake in `dir` at any size, and that size.
pub fn newest(dir: &Path, tag: &str) -> Option<(PathBuf, (u32, u32))> {
    std::fs::read_dir(dir)
        .ok()?
        .flatten()
        .filter_map(|e| {
            let name = e.file_name().into_string().ok()?;
            let (w, h) = name.strip_prefix(tag)?.strip_suffix(".bgra")?.split_once('x')?;
            let at = e.metadata().and_then(|m| m.modified()).ok()?;
            Some((at, e.path(), (w.parse().ok()?, h.parse().ok()?)))
        })
        .max_by_key(|(at, _, _)| *at)
        .map(|(_, f, size)| (f, size))
}

/// Written sealed, whole or not at all; then `tag`'s sizes past the newest `keep` go. Without the
/// cache key nothing is kept.
pub fn store(file: &Path, img: &image::RgbaImage, tag: &str, keep: usize) {
    let Some(dir) = file.parent() else { return };
    let mut bytes = zeroize::Zeroizing::new(Vec::with_capacity(8 + img.as_raw().len()));
    bytes.extend_from_slice(&img.width().to_le_bytes());
    bytes.extend_from_slice(&img.height().to_le_bytes());
    bytes.extend_from_slice(img.as_raw());
    let written = std::fs::create_dir_all(dir).map_err(|e| e.to_string()).and_then(|_| vector_core::vcache::write(file, &bytes));
    if let Err(e) = written {
        vector_core::log_warn!("[Bakes] not kept: {e}");
        return;
    }
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    let mut sizes: Vec<(std::time::SystemTime, PathBuf)> = entries
        .flatten()
        .filter(|e| e.file_name().to_str().is_some_and(|n| n.starts_with(tag) && n.ends_with(".bgra")))
        .map(|e| (e.metadata().and_then(|m| m.modified()).unwrap_or(std::time::UNIX_EPOCH), e.path()))
        .collect();
    sizes.sort_by(|a, b| b.0.cmp(&a.0));
    for (_, old) in sizes.into_iter().skip(keep) {
        let _ = std::fs::remove_file(old);
    }
}

/// Everything in `dir` that no `tags` entry starts, gone; a write in flight is left be.
pub fn sweep(dir: &Path, tags: &[String]) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for e in entries.flatten() {
        let Some(name) = e.file_name().to_str().map(str::to_string) else { continue };
        if name.ends_with(".vtmp") {
            continue;
        }
        if !tags.iter().any(|t| name.starts_with(t.as_str())) {
            let _ = std::fs::remove_file(e.path());
        }
    }
}

/// `src` covering `w`×`h`, centred, scaled up `over` so a blurred edge falls outside.
pub fn cover(src: &image::RgbaImage, (w, h): (u32, u32), over: f32) -> image::RgbaImage {
    let (sw, sh) = (src.width() as f32, src.height() as f32);
    let scale = (w as f32 / sw).max(h as f32 / sh) * over;
    let (cw, ch) = ((w as f32 / scale).min(sw), (h as f32 / scale).min(sh));
    let (cx, cy) = (((sw - cw) / 2.) as u32, ((sh - ch) / 2.) as u32);
    let crop = image::imageops::crop_imm(src, cx, cy, (cw.round() as u32).max(1), (ch.round() as u32).max(1)).to_image();
    gpui::resample(&crop, w, h, image::imageops::FilterType::Triangle)
}

/// A process-wide data folder for tests, so the cache key exists and bakes are sealed.
#[cfg(test)]
pub fn test_data_dir() {
    static DIR: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();
    DIR.get_or_init(|| {
        let dir = std::env::temp_dir().join(format!("vector-gpui-test-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        vector_core::db::set_app_data_dir(dir.clone());
        dir
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_bake_is_sealed_and_an_unsealed_one_is_dropped() {
        test_data_dir();
        let dir = std::env::temp_dir().join(format!("vector-bakes-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let img = image::RgbaImage::from_pixel(3, 2, image::Rgba([1, 2, 3, 255]));
        let f = file(&dir, "abc-", (3, 2));
        store(&f, &img, "abc-", 2);
        assert!(vector_core::vcache::is_sealed(&f));
        assert!(!std::fs::read(&f).unwrap().windows(4).any(|w| w == [1, 2, 3, 255]));
        assert_eq!(load(&f, Some((3, 2))).unwrap().as_raw(), img.as_raw());
        assert_eq!(newest(&dir, "abc-").map(|(_, s)| s), Some((3, 2)));
        let plain = file(&dir, "old-", (1, 1));
        std::fs::write(&plain, [1u8, 0, 0, 0, 1, 0, 0, 0, 9, 9, 9, 9]).unwrap();
        assert!(load(&plain, Some((1, 1))).is_none());
        assert!(!plain.exists());
        sweep(&dir, &[]);
        assert!(!f.exists());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
