//! The fork's bordered atlas tiles: what the five region writes and the padded copy put in the
//! texture, and the tile handed back.

use gpui::{
    AtlasKey, Bounds, DevicePixels, HeadlessAtlas, ImageId, PADDED_IN_SCRATCH, PlatformAtlas, Point, RenderImageParams, Size,
    edge_padded_into, write_edge_padded,
};
use std::borrow::Cow;

fn size(w: usize, h: usize) -> Size<DevicePixels> {
    Size { width: DevicePixels(w as i32), height: DevicePixels(h as i32) }
}

/// The border texel by texel, written the obvious way.
fn padded_by_hand(bytes: &[u8], w: usize, h: usize, bpp: usize) -> Vec<u8> {
    let mut out = Vec::new();
    for y in 0..h + 2 {
        for x in 0..w + 2 {
            let (sx, sy) = (x.clamp(1, w) - 1, y.clamp(1, h) - 1);
            out.extend_from_slice(&bytes[(sy * w + sx) * bpp..][..bpp]);
        }
    }
    out
}

#[test]
fn padded_writes_fill_the_tile_as_the_padded_copy_would() {
    for bpp in [1, 4] {
        for (w, h) in [(1, 1), (1, 9), (9, 1), (3, 2), (131, 127), (1, 20_000), (20_000, 1), (300, 200)] {
            let bytes: Vec<u8> = (0..w * h * bpp).map(|i| (i * 7 + i / 251) as u8).collect();
            let want = padded_by_hand(&bytes, w, h, bpp);
            let mut copy = Vec::new();
            edge_padded_into(&mut copy, &bytes, size(w, h), bpp);
            assert!(copy == want, "{w}x{h}x{bpp}: padded copy");

            let (pw, ph) = (w + 2, h + 2);
            let mut scratch = Vec::new();
            let mut tile = vec![0u8; pw * ph * bpp];
            let mut covered = vec![0u8; pw * ph];
            let mut writes = 0;
            write_edge_padded(&bytes, size(w, h), bpp, &mut scratch, |at, part, src| {
                let (x0, y0) = (at.x.0 as usize, at.y.0 as usize);
                let (rw, rh) = (part.width.0 as usize, part.height.0 as usize);
                assert!(x0 + rw <= pw && y0 + rh <= ph);
                assert_eq!(src.len(), rw * rh * bpp, "rows are tightly packed");
                for y in 0..rh {
                    let to = ((y0 + y) * pw + x0) * bpp;
                    tile[to..to + rw * bpp].copy_from_slice(&src[y * rw * bpp..][..rw * bpp]);
                    covered[(y0 + y) * pw + x0..][..rw].iter_mut().for_each(|c| *c += 1);
                }
                writes += 1;
                Ok(())
            })
            .unwrap();
            assert!(tile == want, "{w}x{h}x{bpp}: written tile");
            assert!(covered.iter().all(|&c| c == 1), "{w}x{h}x{bpp}: every texel written once");
            let small = pw * ph * bpp <= PADDED_IN_SCRATCH;
            assert_eq!(writes, if small { 1 } else { 5 }, "{w}x{h}x{bpp}");
            assert!(scratch.capacity() <= PADDED_IN_SCRATCH.max((h + 2) * bpp), "scratch stays small");
        }
    }
}

fn image_key(id: usize) -> AtlasKey {
    RenderImageParams { image_id: ImageId(id), frame_index: 0 }.into()
}

#[test]
fn padded_tiles_cover_their_pixels_alone() {
    let atlas = HeadlessAtlas::default();
    let pixels = [9u8; 3 * 2 * 4];
    let tile = atlas
        .get_or_insert_padded_with(image_key(1), &mut || Ok(Some((size(3, 2), Cow::Borrowed(&pixels[..])))))
        .unwrap()
        .unwrap();
    assert_eq!(tile.bounds, Bounds { origin: Point { x: DevicePixels(1), y: DevicePixels(1) }, size: size(3, 2) });
    let again = atlas.get_or_insert_padded_with(image_key(1), &mut || panic!("a cache hit builds nothing")).unwrap();
    assert_eq!(again, Some(tile));
    assert_eq!(atlas.get_or_insert_with(image_key(1), &mut || Ok(None)).unwrap(), Some(tile));

    let short = atlas
        .get_or_insert_padded_with(image_key(2), &mut || Ok(Some((size(3, 2), Cow::Borrowed(&pixels[..4])))))
        .unwrap()
        .unwrap();
    assert_eq!(short.bounds, Bounds { origin: Point::default(), size: size(3, 2) });
}
