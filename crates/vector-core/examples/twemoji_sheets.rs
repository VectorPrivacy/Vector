//! Packs the stock emoji the picker shows into a few PNG sheets, for
//! `scripts/build-twemoji-sheets.mjs`: `twemoji_sheets <list> <svg dir> <out dir> <cell px> <per sheet> <cols>`.
//! `list` names one twemoji file per line in the picker's order; sheet `n` holds entries
//! `n * per ..`, left to right, then down.

use std::path::Path;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let [_, list, svg_dir, out_dir, cell, per, cols] = &args[..] else {
        eprintln!("usage: twemoji_sheets <list> <svg dir> <out dir> <cell px> <per sheet> <cols>");
        std::process::exit(2);
    };
    let (cell, per, cols): (u32, usize, u32) = (cell.parse().unwrap(), per.parse().unwrap(), cols.parse().unwrap());
    let (svg_dir, out_dir) = (Path::new(svg_dir), Path::new(out_dir));
    let text = std::fs::read_to_string(list).unwrap();
    let icons: Vec<&str> = text.lines().filter(|l| !l.is_empty()).collect();
    std::fs::create_dir_all(out_dir).unwrap();
    for (n, chunk) in icons.chunks(per).enumerate() {
        let rows = chunk.len().div_ceil(cols as usize) as u32;
        let mut sheet = image::RgbaImage::new(cols * cell, rows * cell);
        for (i, icon) in chunk.iter().enumerate() {
            let svg = std::fs::read(svg_dir.join(format!("{icon}.svg"))).unwrap_or_else(|e| panic!("{icon}: {e}"));
            let r = vector_core::svg::rasterize(&svg, cell).unwrap_or_else(|e| panic!("{icon}: {e}"));
            let x = (i as u32 % cols) * cell + (cell - r.width) / 2;
            let y = (i as u32 / cols) * cell + (cell - r.height) / 2;
            let glyph = image::RgbaImage::from_raw(r.width, r.height, r.rgba).unwrap();
            image::imageops::overlay(&mut sheet, &glyph, x.into(), y.into());
        }
        sheet.save(out_dir.join(format!("{n}.png"))).unwrap();
    }
    println!("{} emoji in {} sheets of {per} at {cell} px", icons.len(), icons.len().div_ceil(per));
}
