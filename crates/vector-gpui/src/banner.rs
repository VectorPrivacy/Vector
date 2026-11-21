//! Community banners as sealed bakes at the list's drawn size: an open reads its banner whole in
//! the first frame, and none is held in memory while its Community is closed.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use gpui::RenderImage;

use crate::bakes;
use crate::data::Store;

/// Sizes kept per banner: the list's width moves rarely.
const KEEP: usize = 2;

/// The banner's pixels at `width` points (the list draws at 238:70).
pub fn size(width: f32) -> (u32, u32) {
    let w = (width * 2.).round().max(1.);
    (w as u32, ((w * 70. / 238.).round() as u32).max(1))
}

fn tag(path: &Path) -> String {
    format!("{:016x}-", bakes::source_hash(path))
}

/// The account's banner bakes.
pub fn dir(npub: &str) -> Option<PathBuf> {
    vector_core::db::account_dir(npub).ok().map(|d| d.join(".bakes").join("banners"))
}

/// The banner at `path` for a list `width` wide, read from its bake on first sight; `None`
/// while it has none.
pub fn shown(store: &Store, path: &Path, width: f32) -> Option<Arc<RenderImage>> {
    let mut slot = store.banner_shown.borrow_mut();
    if let Some((_, image)) = slot.as_ref().filter(|(p, _)| p == path) {
        return image.clone();
    }
    let image = store.banner_bakes.as_deref().and_then(|dir| read(dir, path, size(width))).map(Arc::new);
    if let Some((_, Some(old))) = slot.replace((path.to_path_buf(), image.clone())) {
        store.banner_retired.borrow_mut().push(old);
    }
    image
}

/// Its bake at `want`, else its newest at any size for the GPU to stretch.
fn read(dir: &Path, path: &Path, want: (u32, u32)) -> Option<RenderImage> {
    let tag = tag(path);
    let exact = bakes::file(dir, &tag, want);
    let (file, size) = if exact.exists() { (exact, want) } else { bakes::newest(dir, &tag)? };
    Some(RenderImage::new(vec![image::Frame::new(bakes::load(&file, size)?)]))
}

/// Bakes `show` lacks at `want`, and every bake no banner in `all` still has, gone. Disk only;
/// whether anything was baked.
pub fn bake_all(dir: PathBuf, show: Vec<PathBuf>, all: Vec<PathBuf>, want: (u32, u32)) -> bool {
    let _trim = crate::trim::OnDrop;
    let mut baked = false;
    for path in show {
        let tag = tag(&path);
        let file = bakes::file(&dir, &tag, want);
        if file.exists() {
            continue;
        }
        let Ok(bytes) = crate::pic::read(&path) else { continue };
        // A moving banner keeps to the decoder that animates it.
        if bytes.starts_with(b"GIF8") {
            continue;
        }
        let ext = crate::backend::inline::sniff(&bytes).unwrap_or("png");
        let Some((pic, ..)) = crate::media::decode(&bytes, ext, (want.0 * 2, want.0 * 2)) else { continue };
        if pic.frame_count() > 1 {
            continue;
        }
        let px = pic.size(0);
        let Some(src) = pic.as_bytes(0).and_then(|b| image::RgbaImage::from_raw(px.width.0 as u32, px.height.0 as u32, b.to_vec())) else { continue };
        bakes::store(&file, &bakes::cover(&src, want, 1.), &tag, KEEP);
        baked = true;
    }
    bakes::sweep(&dir, &all.iter().map(|p| tag(p)).collect::<Vec<_>>());
    baked
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_banner_bakes_sealed_at_its_size_and_goes_with_its_picture() {
        bakes::test_data_dir();
        let root = std::env::temp_dir().join(format!("vector-banner-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let src = root.join("b.png");
        image::RgbaImage::from_pixel(952, 280, image::Rgba([200, 40, 40, 255])).save(&src).unwrap();
        let dir = root.join("bakes");
        let want = size(239.);
        assert!(bake_all(dir.clone(), vec![src.clone()], vec![src.clone()], want));
        assert!(!bake_all(dir.clone(), vec![src.clone()], vec![src.clone()], want), "baked once");
        let img = read(&dir, &src, want).expect("read back");
        assert_eq!(img.size(0).width.0 as u32, want.0);
        assert!(read(&dir, &src, size(300.)).is_some(), "another width stretches the newest");
        bake_all(dir.clone(), Vec::new(), Vec::new(), want);
        assert!(read(&dir, &src, want).is_none(), "a banner no Community shows takes its bakes");
        let _ = std::fs::remove_dir_all(&root);
    }
}
