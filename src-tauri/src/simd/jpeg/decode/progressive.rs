//! Progressive (SOF2) decoding: every scan's coefficients accumulate per block (ITU T.81
//! Annex G), then the blocks go through the same IDCT, DCT scaling and colour stages as a
//! baseline image, in parallel across block rows.

#[cfg(target_arch = "aarch64")]
use super::super::Neon;
#[cfg(target_arch = "x86_64")]
use super::super::Avx2;
use super::super::{Backend, Scalar, Simd, S_OF};
use super::{
    be16, dc_block, dc_only, destuff, idct_block, idct_scaled, idct_square, read_dht, to_pixels, Bits, DecodeError,
    Decoded, Entropy, Frame, Header, Huff, Plane,
};
use rayon::prelude::*;

use DecodeError::{Corrupt, Unsupported};

/// One component's coefficients over its padded MCU grid. DC and AC live apart so DC scans
/// and each component's AC scans can decode in parallel without sharing memory.
struct Coefs {
    dc: Vec<i16>,
    /// 64 per block in kernel order; index 0 unused.
    ac: Vec<i16>,
    /// Blocks per row, as the MCU grid pads it.
    bw: usize,
    /// Blocks a non-interleaved scan visits: the component's own extent.
    vis_w: usize,
    vis_h: usize,
}

#[cfg(test)]
thread_local! {
    /// Decode every scan, as the reference the skipping is checked against.
    static KEEP_ALL: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// A scan with the tables and restart interval in force where it appeared.
struct Scan {
    spec: ScanSpec,
    entropy: Entropy,
    dc: [Option<Box<Huff>>; 4],
    ac: [Option<Box<Huff>>; 4],
    restart: usize,
}

/// Block geometry a scan walks, per component.
#[derive(Clone, Copy)]
struct Grid {
    bw: usize,
    vis_w: usize,
    vis_h: usize,
}

struct ScanSpec {
    /// Frame component indices in this scan, in scan order.
    comps: Vec<usize>,
    /// DC and AC table ids per scan component.
    tables: Vec<(usize, usize)>,
    ss: usize,
    se: usize,
    ah: u32,
    al: u32,
}

pub(super) fn decode(data: &[u8], header: Header, denom: usize, backend: Backend) -> Result<Decoded, DecodeError> {
    let mut frame = Frame::new(&header, denom)?;
    let (hmax, vmax) = (frame.comps[0].h, frame.comps[0].v);
    let mut coefs: Vec<Coefs> = frame
        .comps
        .iter()
        .map(|c| {
            let (bw, bh) = (frame.mcux * c.h, frame.mcuy * c.v);
            let (cw, ch) = ((header.width * c.h).div_ceil(hmax), (header.height * c.v).div_ceil(vmax));
            Coefs { dc: vec![0; bw * bh], ac: vec![0; bw * bh * 64], bw, vis_w: cw.div_ceil(8), vis_h: ch.div_ceil(8) }
        })
        .collect();
    let (mut dc, mut ac) = (header.dc, header.ac);
    let mut restart = header.restart;
    let mut scans = Vec::new();
    let mut i = header.first_scan;
    loop {
        if data.get(i) != Some(&0xFF) {
            return Err(Corrupt("expected a marker"));
        }
        while data.get(i) == Some(&0xFF) {
            i += 1;
        }
        let marker = *data.get(i).ok_or(Corrupt("truncated file"))?;
        i += 1;
        match marker {
            0xD9 => break,
            0x01 | 0xD0..=0xD7 => continue,
            _ => {}
        }
        let len = be16(data, i)?;
        let seg = data.get(i + 2..i + len).filter(|_| len >= 2).ok_or(Corrupt("truncated segment"))?;
        i += len;
        match marker {
            0xC4 => read_dht(seg, &mut dc, &mut ac)?,
            0xDD => restart = be16(seg, 0)?,
            0xDB => return Err(Unsupported("quantisation table between scans")),
            0xDC => return Err(Unsupported("DNL")),
            0xDA => {
                let spec = scan_spec(seg, &frame)?;
                let entropy = destuff(&data[i..]);
                i += entropy.stop;
                let more = entropy.next_marker.is_some();
                scans.push(Scan { spec, entropy, dc: dc.clone(), ac: ac.clone(), restart });
                if !more {
                    break;
                }
            }
            _ => {}
        }
    }
    frame.restart = restart;

    // Scaled, the IDCT reads only each block's lowest `bw` x `bh` frequencies. An AC scan
    // covering none of them is dropped, unless a kept refinement scan still reads its bits.
    let needed = |c: usize, k: usize| {
        let ((bw, bh), s) = (frame.block_size[c], usize::from(S_OF[k]));
        s / 8 < bw && s % 8 < bh
    };
    let mut keep = vec![true; scans.len()];
    for i in (0..scans.len()).rev() {
        let s = &scans[i].spec;
        if s.ss == 0 {
            continue;
        }
        let c = s.comps[0];
        let fed = scans[i + 1..].iter().zip(&keep[i + 1..]).any(|(t, &kept)| {
            kept && t.spec.ss > 0 && t.spec.comps[0] == c && t.spec.ss <= s.se && s.ss <= t.spec.se
        });
        keep[i] = fed || (s.ss..=s.se).any(|k| needed(c, k));
        #[cfg(test)]
        if KEEP_ALL.with(std::cell::Cell::get) {
            keep[i] = true;
        }
    }
    let scans: Vec<Scan> = scans.into_iter().zip(keep).filter_map(|(s, kept)| kept.then_some(s)).collect();

    // DC scans touch only coefficient 0 of every component, AC scans only their one
    // component's 1..63: each group decodes in order, the groups in parallel.
    let grids: Vec<Grid> = coefs.iter().map(|c| Grid { bw: c.bw, vis_w: c.vis_w, vis_h: c.vis_h }).collect();
    let (mut dcs, mut acs): (Vec<&mut Vec<i16>>, Vec<&mut Vec<i16>>) = coefs.iter_mut().map(|c| (&mut c.dc, &mut c.ac)).unzip();
    let dc_scans: Vec<&Scan> = scans.iter().filter(|s| s.spec.ss == 0).collect();
    let frame_ref = &frame;
    let grids_ref = &grids;
    let jobs: Vec<Box<dyn FnOnce() -> Result<(), DecodeError> + Send + '_>> = std::iter::once(Box::new(move || {
        dc_scans.iter().try_for_each(|s| decode_dc_scan(frame_ref, grids_ref, s, &mut dcs))
    }) as Box<dyn FnOnce() -> Result<(), DecodeError> + Send>)
    .chain(acs.iter_mut().enumerate().map(|(c, ac)| {
        let mine: Vec<&Scan> = scans.iter().filter(|s| s.spec.ss > 0 && s.spec.comps[0] == c).collect();
        let grid = grids[c];
        Box::new(move || mine.iter().try_for_each(|s| decode_ac_scan(grid, s, ac))) as Box<dyn FnOnce() -> Result<(), DecodeError> + Send>
    }))
    .collect();
    jobs.into_par_iter().map(|job| job()).collect::<Result<Vec<()>, DecodeError>>()?;

    let mut planes: Vec<Plane> = frame
        .comps
        .iter()
        .map(|c| {
            let (bw, bh) = frame.block_size[c.index];
            let stride = (frame.mcux * c.h * bw).next_multiple_of(8);
            Plane { data: vec![0; stride * frame.mcuy * c.v * bh], stride }
        })
        .collect();
    for (c, (plane, coef)) in planes.iter_mut().zip(&coefs).enumerate() {
        let (bw, bh) = frame.block_size[c];
        let stride = plane.stride;
        let (q8, qraw) = (&frame.qs[c], &frame.qraw[c]);
        let basis = &frame.basis;
        let rows = coef.ac.par_chunks(coef.bw * 64).zip(coef.dc.par_chunks(coef.bw));
        plane.data.par_chunks_mut(stride * bh).zip(rows).for_each(|(out, (row, dc))| {
            // SAFETY: each backend runs only where `Backend::detect` found its features; `out`
            // holds one block row of `bh` lines at `stride`, `bw` pixels per block.
            unsafe {
                match backend {
                    #[cfg(target_arch = "x86_64")]
                    Backend::Avx2 => idct_row_avx2(row, dc, q8, qraw, basis, bw, bh, out, stride),
                    #[cfg(target_arch = "aarch64")]
                    Backend::Neon => idct_row_neon(row, dc, q8, qraw, basis, bw, bh, out, stride),
                    Backend::Scalar => idct_row::<Scalar>(row, dc, q8, qraw, basis, bw, bh, out, stride),
                }
            }
        });
    }
    Ok(to_pixels(&frame, &planes, backend))
}

fn scan_spec(seg: &[u8], frame: &Frame) -> Result<ScanSpec, DecodeError> {
    let ns = usize::from(*seg.first().ok_or(Corrupt("short scan header"))?);
    let sel = seg.get(1..1 + 2 * ns).ok_or(Corrupt("short scan header"))?;
    let tail = seg.get(1 + 2 * ns..4 + 2 * ns).ok_or(Corrupt("short scan header"))?;
    if ns == 0 || ns > frame.comps.len() {
        return Err(Corrupt("scan component count"));
    }
    let mut comps = Vec::with_capacity(ns);
    let mut tables = Vec::with_capacity(ns);
    for s in sel.chunks_exact(2) {
        let c = frame.comps.iter().position(|c| c.id == s[0]).ok_or(Corrupt("scan component"))?;
        if comps.contains(&c) {
            return Err(Corrupt("scan component repeated"));
        }
        let (td, ta) = (usize::from(s[1] >> 4), usize::from(s[1] & 15));
        if td > 3 || ta > 3 {
            return Err(Corrupt("scan table selector"));
        }
        comps.push(c);
        tables.push((td, ta));
    }
    let (ss, se, ah, al) = (usize::from(tail[0]), usize::from(tail[1]), u32::from(tail[2] >> 4), u32::from(tail[2] & 15));
    let dc_scan = ss == 0;
    if se > 63 || ss > se || (dc_scan && se != 0) || (!dc_scan && ns != 1) || ah > 13 || al > 13 {
        return Err(Corrupt("progressive scan parameters"));
    }
    Ok(ScanSpec { comps, tables, ss, se, ah, al })
}

/// Walk a scan's units (MCUs when interleaved, else the component's blocks), restarting
/// where the interval says, and hand each block to `f` with its component and block index.
fn walk_scan(
    frame: &Frame,
    grids: &[Grid],
    scan: &Scan,
    mut f: impl FnMut(&mut Bits, usize, usize, bool),
) -> Result<(), DecodeError> {
    let spec = &scan.spec;
    let e = &scan.entropy;
    let interleaved = spec.comps.len() > 1;
    let (units_x, units_y) = if interleaved {
        (frame.mcux, frame.mcuy)
    } else {
        let g = grids[spec.comps[0]];
        (g.vis_w, g.vis_h)
    };
    let mut b = Bits::new(e);
    let (mut interval, mut left) = (0, scan.restart);
    for uy in 0..units_y {
        for ux in 0..units_x {
            let mut restarted = false;
            if scan.restart > 0 {
                if left == 0 {
                    b.jump(*e.restarts.get(interval).ok_or(Corrupt("missing restart marker"))?);
                    restarted = true;
                    interval += 1;
                    left = scan.restart;
                }
                left -= 1;
            }
            for (k, &c) in spec.comps.iter().enumerate() {
                let (h, v) = if interleaved { (frame.comps[c].h, frame.comps[c].v) } else { (1, 1) };
                for by in 0..v {
                    for bx in 0..h {
                        f(&mut b, k, (uy * v + by) * grids[c].bw + ux * h + bx, restarted);
                        restarted = false;
                    }
                }
            }
            if b.bad {
                return Err(Corrupt("invalid entropy data"));
            }
        }
    }
    Ok(())
}

fn decode_dc_scan(frame: &Frame, grids: &[Grid], scan: &Scan, dcs: &mut [&mut Vec<i16>]) -> Result<(), DecodeError> {
    let spec = &scan.spec;
    let tables: Vec<Option<&Huff>> = if spec.ah == 0 {
        spec.tables.iter().map(|&(td, _)| scan.dc[td].as_deref().ok_or(Corrupt("missing Huffman table")).map(Some)).collect::<Result<_, _>>()?
    } else {
        vec![None; spec.comps.len()]
    };
    let mut pred = [0i32; 4];
    walk_scan(frame, grids, scan, |b, k, block, restarted| {
        if restarted {
            pred = [0; 4];
        }
        let coef = &mut dcs[spec.comps[k]][block];
        match tables[k] {
            Some(d) => dc_first(b, d, &mut pred[k], spec.al, coef),
            None => {
                if b.bits(1) == 1 {
                    *coef |= 1 << spec.al;
                }
            }
        }
    })
}

fn decode_ac_scan(grid: Grid, scan: &Scan, ac: &mut [i16]) -> Result<(), DecodeError> {
    let spec = &scan.spec;
    let a = scan.ac[spec.tables[0].1].as_deref().ok_or(Corrupt("missing Huffman table"))?;
    let mut eobrun = 0u32;
    walk_single(&grid, scan, |b, block, restarted| {
        if restarted {
            eobrun = 0;
        }
        let blk: &mut [i16; 64] = (&mut ac[block * 64..block * 64 + 64]).try_into().unwrap();
        if spec.ah == 0 {
            ac_first(b, a, spec, &mut eobrun, blk);
        } else {
            ac_refine(b, a, spec, &mut eobrun, blk);
        }
    })
}

/// [`walk_scan`] for a one-component scan, which needs no frame geometry.
fn walk_single(grid: &Grid, scan: &Scan, mut f: impl FnMut(&mut Bits, usize, bool)) -> Result<(), DecodeError> {
    let e = &scan.entropy;
    let mut b = Bits::new(e);
    let (mut interval, mut left) = (0, scan.restart);
    for by in 0..grid.vis_h {
        for bx in 0..grid.vis_w {
            let mut restarted = false;
            if scan.restart > 0 {
                if left == 0 {
                    b.jump(*e.restarts.get(interval).ok_or(Corrupt("missing restart marker"))?);
                    restarted = true;
                    interval += 1;
                    left = scan.restart;
                }
                left -= 1;
            }
            f(&mut b, by * grid.bw + bx, restarted);
            if b.bad {
                return Err(Corrupt("invalid entropy data"));
            }
        }
    }
    Ok(())
}

#[inline(always)]
fn dc_first(b: &mut Bits, d: &Huff, pred: &mut i32, al: u32, coef: &mut i16) {
    if b.cnt < 32 {
        b.refill();
    }
    let s = u32::from(d.decode(b));
    if s > 15 {
        b.bad = true;
        return;
    }
    if s > 0 {
        *pred = pred.wrapping_add(b.extend(s));
    }
    *coef = pred.wrapping_shl(al) as i16;
}

/// First pass over an AC band: values scaled by `2^al`, with end-of-band runs across blocks.
#[inline(always)]
fn ac_first(b: &mut Bits, a: &Huff, spec: &ScanSpec, eobrun: &mut u32, blk: &mut [i16; 64]) {
    if *eobrun > 0 {
        *eobrun -= 1;
        return;
    }
    let mut k = spec.ss;
    while k <= spec.se {
        if b.cnt < 32 {
            b.refill();
        }
        let rs = a.decode(b);
        let (r, s) = (usize::from(rs >> 4), u32::from(rs & 15));
        if s == 0 {
            if r < 15 {
                *eobrun = (1 << r) - 1 + b.bits(r as u32);
                return;
            }
            k += 16;
            continue;
        }
        k += r;
        if k > 63 {
            b.bad = true;
            return;
        }
        blk[S_OF[k] as usize] = b.extend(s).wrapping_shl(spec.al) as i16;
        k += 1;
    }
}

/// Refinement of an AC band: one correction bit per coefficient already nonzero, newly
/// nonzero coefficients at `±2^al`, and end-of-band runs that still carry correction bits.
#[inline(always)]
fn ac_refine(b: &mut Bits, a: &Huff, spec: &ScanSpec, eobrun: &mut u32, blk: &mut [i16; 64]) {
    let p1 = 1i16.wrapping_shl(spec.al);
    let m1 = (-1i16).wrapping_shl(spec.al);
    let refine = |b: &mut Bits, z: &mut i16| {
        if b.bits(1) == 1 && *z & p1 == 0 {
            *z = z.wrapping_add(if *z >= 0 { p1 } else { m1 });
        }
    };
    let mut k = spec.ss;
    if *eobrun == 0 {
        while k <= spec.se {
            if b.cnt < 32 {
                b.refill();
            }
            let rs = a.decode(b);
            let (mut r, s) = (usize::from(rs >> 4), rs & 15);
            let mut value = 0i16;
            if s != 0 {
                if s != 1 {
                    b.bad = true;
                    return;
                }
                value = if b.bits(1) == 1 { p1 } else { m1 };
            } else if r != 15 {
                *eobrun = (1 << r) + b.bits(r as u32);
                break;
            }
            // Skip `r` still-zero coefficients, refining the nonzero ones passed on the way;
            // the next zero takes `value`.
            while k <= spec.se {
                let z = &mut blk[S_OF[k] as usize];
                if *z != 0 {
                    refine(b, z);
                } else if r == 0 {
                    if value != 0 {
                        *z = value;
                    }
                    k += 1;
                    break;
                } else {
                    r -= 1;
                }
                k += 1;
            }
        }
    }
    if *eobrun > 0 {
        while k <= spec.se {
            let z = &mut blk[S_OF[k] as usize];
            if *z != 0 {
                refine(b, z);
            }
            k += 1;
        }
        *eobrun -= 1;
    }
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2")]
#[allow(clippy::too_many_arguments)]
unsafe fn idct_row_avx2(row: &[i16], dc: &[i16], q8: &[f32; 64], qraw: &[f32; 64], basis: &[[f32; 64]; 4], bw: usize, bh: usize, out: &mut [u8], stride: usize) {
    idct_row::<Avx2>(row, dc, q8, qraw, basis, bw, bh, out, stride)
}

#[cfg(target_arch = "aarch64")]
#[target_feature(enable = "neon")]
#[allow(clippy::too_many_arguments)]
unsafe fn idct_row_neon(row: &[i16], dc: &[i16], q8: &[f32; 64], qraw: &[f32; 64], basis: &[[f32; 64]; 4], bw: usize, bh: usize, out: &mut [u8], stride: usize) {
    idct_row::<Neon>(row, dc, q8, qraw, basis, bw, bh, out, stride)
}

/// One row of coefficient blocks into `bh` lines of pixels, `bw` pixels a block.
#[inline(always)]
#[allow(clippy::too_many_arguments)]
unsafe fn idct_row<S: Simd>(row: &[i16], dc: &[i16], q8: &[f32; 64], qraw: &[f32; 64], basis: &[[f32; 64]; 4], bw: usize, bh: usize, out: &mut [u8], stride: usize) {
    for (bx, (ac, &dc)) in row.chunks_exact(64).zip(dc).enumerate() {
        let mut blk: [i16; 64] = ac.try_into().unwrap();
        blk[0] = dc;
        let blk = &blk;
        let dst = out.as_mut_ptr().add(bx * bw);
        if dc_only(blk, bw, bh) {
            dc_block(blk[0], q8[0], dst, stride, bw, bh);
        } else if (bw, bh) == (8, 8) {
            idct_block::<S>(blk, q8, dst, stride);
        } else if bw == bh {
            match bw {
                4 => idct_square::<4>(blk, qraw, dst, stride),
                2 => idct_square::<2>(blk, qraw, dst, stride),
                _ => idct_square::<1>(blk, qraw, dst, stride),
            }
        } else {
            idct_scaled(blk, qraw, basis, bw, bh, dst, stride);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::{decode_with, Backend};

    /// Each with libjpeg-turbo's decode of it: zune-jpeg before 0.5.15 misdecodes
    /// progressive files with restart markers, so it can't be the reference here.
    const FIXTURES: [(&str, &[u8], &[u8]); 3] = [
        (
            "4:2:0 with restarts",
            include_bytes!("../testdata/progressive_420_restarts.jpg"),
            include_bytes!("../testdata/progressive_420_restarts.png"),
        ),
        ("4:4:4", include_bytes!("../testdata/progressive_444.jpg"), include_bytes!("../testdata/progressive_444.png")),
        ("greyscale", include_bytes!("../testdata/progressive_grey.jpg"), include_bytes!("../testdata/progressive_grey.png")),
    ];

    fn psnr(a: &[u8], b: &[u8]) -> f64 {
        let mse = a.iter().zip(b).map(|(x, y)| (f64::from(*x) - f64::from(*y)).powi(2)).sum::<f64>() / a.len() as f64;
        10.0 * (255.0f64.powi(2) / mse.max(1e-9)).log10()
    }

    /// Fixtures written with libjpeg-turbo's default progression: DC first and refine, AC bands
    /// with successive approximation, AC refinement.
    #[test]
    fn decodes_progressive_like_the_reference() {
        for (name, jpeg, reference) in FIXTURES {
            let ours = decode_with(jpeg, Backend::detect(), None, 0).unwrap();
            let img = image::load_from_memory_with_format(reference, image::ImageFormat::Png).unwrap();
            let theirs = if ours.channels == 1 { img.to_luma8().into_raw() } else { img.to_rgb8().into_raw() };
            assert_eq!((ours.width, ours.height), (img.width(), img.height()), "{name}");
            let p = psnr(&ours.pixels, &theirs);
            assert!(p > 45.0, "{name}: {p:.1} dB from libjpeg-turbo");
            assert!(decode_with(jpeg, Backend::Scalar, None, 0).unwrap().pixels == ours.pixels, "{name}: backends differ");
            let half = decode_with(jpeg, Backend::detect(), None, 30).unwrap();
            assert_eq!((half.width, half.height), (ours.width.div_ceil(2), ours.height.div_ceil(2)), "{name} at 1/2");
        }
    }

    #[test]
    fn skipped_scans_never_change_a_scaled_decode() {
        use super::super::decode_at_least;
        let mut files: Vec<Vec<u8>> = FIXTURES.iter().map(|f| f.1.to_vec()).collect();
        if let Ok(dir) = std::env::var("PROGRESSIVE_DIR") {
            files.extend(std::fs::read_dir(dir).unwrap().flatten().map(|e| std::fs::read(e.path()).unwrap()));
        }
        for (n, jpeg) in files.iter().enumerate() {
            let long = image::load_from_memory(jpeg).map(|i| i.width().max(i.height())).unwrap_or(64);
            for min in [long / 8, long / 4, long / 2, long] {
                let skipped = decode_at_least(jpeg, min).unwrap();
                super::KEEP_ALL.with(|k| k.set(true));
                let all = decode_at_least(jpeg, min).unwrap();
                super::KEEP_ALL.with(|k| k.set(false));
                assert_eq!((skipped.width, skipped.height), (all.width, all.height));
                assert!(skipped.pixels == all.pixels, "file {n} at >= {min} px");
            }
        }
    }

    #[test]
    fn damaged_progressive_input_never_panics() {
        let mut s = 0x2545_f491u32;
        for (_, jpeg, _) in FIXTURES {
            for round in 0..200 {
                let mut bad = jpeg.to_vec();
                s ^= s << 13;
                s ^= s >> 17;
                s ^= s << 5;
                if round % 4 == 0 {
                    bad.truncate(s as usize % jpeg.len());
                } else {
                    for _ in 0..1 + round % 5 {
                        s ^= s << 13;
                        s ^= s >> 17;
                        s ^= s << 5;
                        let i = s as usize % bad.len();
                        bad[i] ^= (s >> 24) as u8 | 1;
                    }
                }
                let _ = decode_with(&bad, Backend::detect(), None, 0);
            }
        }
    }
}
