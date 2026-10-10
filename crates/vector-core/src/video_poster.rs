//! A web video's opening, enough for a webview to show its first frame, fetched as byte
//! ranges rather than the whole file.
//!
//! WebM starts with its first frames. MP4 and MOV need their index (`moov`) too, which a
//! camera often writes after the data: then it is fetched from where it sits and moved to
//! the front, its chunk offsets shifted by its own length, as `qt-faststart` does. What
//! comes back is a short file that plays its first frames and stops.

use std::time::Duration;

/// The first request: an index at the front and a first frame usually fit in it.
const HEAD_BYTES: u64 = 512 * 1024;
/// WebM's first cluster holds its keyframe; this covers it for anything chat-sized.
const WEBM_BYTES: u64 = 2 * 1024 * 1024;
/// Past this a first frame isn't worth the bytes; the video keeps its tap to play.
const MAX_PREFIX_BYTES: u64 = 8 * 1024 * 1024;
const MAX_MOOV_BYTES: u64 = 4 * 1024 * 1024;
/// After the first sample: the few frames a seek to the opening decodes.
const FRAME_MARGIN: u64 = 256 * 1024;
/// Boxes walked past the data looking for the index.
const MAX_BOX_HOPS: usize = 8;

pub struct Poster {
    pub bytes: Vec<u8>,
    pub ext: &'static str,
}

/// The opening of the video at `url`, through the transport and the privacy proxy like
/// any other media. An error means no frame is shown, never a broken one.
pub async fn fetch(url: &str) -> Result<Poster, String> {
    crate::net::validate_url_not_private(url).map_err(str::to_string)?;
    poster_from(&Web(url)).await
}

/// Where the bytes come from: the web, or a file in a test.
trait Source {
    /// Bytes `start..end`, and the file's size when known. A source that can't seek may
    /// answer only from the top, so `start` must be 0 then.
    async fn range(&self, start: u64, end: u64) -> Result<Fetched, String>;
}

async fn poster_from(src: &impl Source) -> Result<Poster, String> {
    let head = src.range(0, HEAD_BYTES).await?;
    let total = head.total;
    let mut bytes = head.bytes;
    if bytes.starts_with(&[0x1A, 0x45, 0xDF, 0xA3]) {
        if (bytes.len() as u64) < WEBM_BYTES && total.is_none_or(|t| bytes.len() as u64 != t) {
            extend(src, &mut bytes, WEBM_BYTES, total).await?;
        }
        return Ok(Poster { bytes, ext: "webm" });
    }
    if bytes.len() < 12 || &bytes[4..8] != b"ftyp" {
        return Err("Not a video".into());
    }
    let ext = if &bytes[8..12] == b"qt  " { "mov" } else { "mp4" };
    if total.is_some_and(|t| bytes.len() as u64 >= t) {
        return Ok(Poster { bytes, ext });
    }

    let layout = top_level(&bytes);
    let mdat_box = layout.iter().find(|b| &b.kind == b"mdat").ok_or("No video data")?;
    let (mdat, mdat_end) = (mdat_box.offset, mdat_box.size.map(|s| mdat_box.offset + s));
    let (moov_offset, moov) = match layout.iter().find(|b| &b.kind == b"moov") {
        Some(b) => {
            let size = b.size.ok_or("No video index")?;
            if size > MAX_MOOV_BYTES {
                return Err("Video index too large".into());
            }
            if b.offset + size > bytes.len() as u64 {
                extend(src, &mut bytes, b.offset + size, total).await?;
            }
            (b.offset, bytes[b.offset as usize..(b.offset + size) as usize].to_vec())
        }
        None => find_moov(src, &layout, total).await?,
    };

    let first = first_video_sample(&moov).ok_or("No video track")?;
    let need = first.0.checked_add(first.1).ok_or("Bad video index")?;
    if need > MAX_PREFIX_BYTES {
        return Err("The first frame is too far in".into());
    }
    let want = total.map_or(need + FRAME_MARGIN, |t| t.min(need + FRAME_MARGIN));
    if (bytes.len() as u64) < want {
        extend(src, &mut bytes, want, total).await?;
    }
    if (bytes.len() as u64) < need {
        return Err("The first frame didn't arrive".into());
    }
    bytes.truncate(want.min(bytes.len() as u64) as usize);

    if moov_offset < mdat {
        return Ok(Poster { bytes, ext });
    }
    // The index's old place, when the prefix reaches it, goes: one index per file.
    if let Some(end) = mdat_end {
        bytes.truncate(end.min(bytes.len() as u64) as usize);
    }
    Ok(Poster { bytes: faststart(&bytes, mdat, moov)?, ext })
}

/// The index moved in front of the data, every chunk offset in it shifted by its length.
fn faststart(prefix: &[u8], mdat: u64, mut moov: Vec<u8>) -> Result<Vec<u8>, String> {
    let shift = moov.len() as u64;
    shift_chunk_offsets(&mut moov, shift)?;
    let split = mdat as usize;
    if split > prefix.len() {
        return Err("No video data".into());
    }
    let mut out = Vec::with_capacity(prefix.len() + moov.len());
    out.extend_from_slice(&prefix[..split]);
    out.extend_from_slice(&moov);
    out.extend_from_slice(&prefix[split..]);
    Ok(out)
}

struct Fetched {
    bytes: Vec<u8>,
    total: Option<u64>,
}

struct Web<'a>(&'a str);

impl Source for Web<'_> {
    /// A server that ignores the range is read from the top and cut off.
    async fn range(&self, start: u64, end: u64) -> Result<Fetched, String> {
        range(self.0, start, end).await
    }
}

async fn range(url: &str, start: u64, end: u64) -> Result<Fetched, String> {
    use futures_util::StreamExt;
    let timeout = crate::transport::budget(crate::transport::Op::HttpTotal, Duration::from_secs(20));
    let client = crate::net::build_http_client(timeout)?;
    let resp = crate::net::proxied_request(&client, reqwest::Method::GET, url)
        .await
        .header(reqwest::header::RANGE, format!("bytes={start}-{}", end - 1))
        .send()
        .await
        .map_err(|e| e.to_string())?;
    let status = resp.status();
    let ranged = status == reqwest::StatusCode::PARTIAL_CONTENT;
    if !ranged && !(status.is_success() && start == 0) {
        return Err(format!("The video host answered {status}"));
    }
    let total = if ranged {
        resp.headers()
            .get(reqwest::header::CONTENT_RANGE)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.rsplit('/').next())
            .and_then(|t| t.parse().ok())
    } else {
        resp.content_length()
    };
    let want = (end - start) as usize;
    let mut bytes = Vec::with_capacity(want.min(1024 * 1024));
    let mut body = resp.bytes_stream();
    while let Some(chunk) = body.next().await {
        bytes.extend_from_slice(&chunk.map_err(|e| e.to_string())?);
        if bytes.len() >= want {
            bytes.truncate(want);
            break;
        }
    }
    Ok(Fetched { bytes, total })
}

/// Grow `bytes` (the file from its start) to `end`, or to the file's end.
async fn extend(src: &impl Source, bytes: &mut Vec<u8>, end: u64, total: Option<u64>) -> Result<(), String> {
    let end = total.map_or(end, |t| t.min(end));
    let have = bytes.len() as u64;
    if have >= end {
        return Ok(());
    }
    if end > MAX_PREFIX_BYTES + MAX_MOOV_BYTES {
        return Err("Too far into the video".into());
    }
    let more = src.range(have, end).await?;
    bytes.extend_from_slice(&more.bytes);
    Ok(())
}

/// The index when it comes after the data: box headers read one hop at a time from the
/// end of the last box the head showed.
async fn find_moov(src: &impl Source, layout: &[BoxAt], total: Option<u64>) -> Result<(u64, Vec<u8>), String> {
    let last = layout.last().ok_or("No video index")?;
    let mut at = last.offset.checked_add(last.size.ok_or("No video index")?).ok_or("Bad video")?;
    for _ in 0..MAX_BOX_HOPS {
        if total.is_some_and(|t| at + 8 > t) {
            break;
        }
        let head = src.range(at, at + 16).await?.bytes;
        let b = parse_header(&head, at).ok_or("No video index")?;
        let size = b.size.ok_or("No video index")?;
        if &b.kind == b"moov" {
            if size > MAX_MOOV_BYTES {
                return Err("Video index too large".into());
            }
            let moov = src.range(at, at + size).await?.bytes;
            if moov.len() as u64 != size {
                return Err("The video index didn't arrive".into());
            }
            return Ok((at, moov));
        }
        at = at.checked_add(size).ok_or("Bad video")?;
    }
    Err("No video index".into())
}

struct BoxAt {
    offset: u64,
    header: u64,
    /// None when the box runs to the end of the file.
    size: Option<u64>,
    kind: [u8; 4],
}

fn parse_header(buf: &[u8], offset: u64) -> Option<BoxAt> {
    let size32 = u32::from_be_bytes(buf.get(0..4)?.try_into().ok()?) as u64;
    let kind: [u8; 4] = buf.get(4..8)?.try_into().ok()?;
    let (header, size) = match size32 {
        0 => (8, None),
        1 => (16, Some(u64::from_be_bytes(buf.get(8..16)?.try_into().ok()?))),
        n => (8, Some(n)),
    };
    if size.is_some_and(|s| s < header) {
        return None;
    }
    Some(BoxAt { offset, header, size, kind })
}

/// The top-level boxes whose headers are in `buf`, the file's start.
fn top_level(buf: &[u8]) -> Vec<BoxAt> {
    let mut out = Vec::new();
    let mut at = 0u64;
    while let Some(b) = buf.get(at as usize..).and_then(|rest| parse_header(rest, at)) {
        let next = b.size.and_then(|s| at.checked_add(s));
        out.push(b);
        match next {
            Some(n) => at = n,
            None => break,
        }
        if out.len() > 64 {
            break;
        }
    }
    out
}

/// The children of the container whose payload is `buf[start..end]`, as (payload start, end, kind).
fn children(buf: &[u8], start: usize, end: usize) -> Vec<(usize, usize, [u8; 4])> {
    let mut out = Vec::new();
    let mut at = start;
    while at + 8 <= end {
        let Some(b) = parse_header(&buf[at..end], at as u64) else { break };
        let size = b.size.map_or(end - at, |s| s as usize);
        if size < b.header as usize || at + size > end {
            break;
        }
        out.push((at + b.header as usize, at + size, b.kind));
        at += size;
    }
    out
}

fn child(buf: &[u8], start: usize, end: usize, kind: &[u8; 4]) -> Option<(usize, usize)> {
    children(buf, start, end).into_iter().find(|c| &c.2 == kind).map(|c| (c.0, c.1))
}

fn be32(buf: &[u8], at: usize) -> Option<u64> {
    Some(u32::from_be_bytes(buf.get(at..at + 4)?.try_into().ok()?) as u64)
}

fn be64(buf: &[u8], at: usize) -> Option<u64> {
    Some(u64::from_be_bytes(buf.get(at..at + 8)?.try_into().ok()?))
}

/// Each track's sample table, as (payload start, end) in `moov`.
fn sample_tables(moov: &[u8]) -> Vec<(usize, usize, bool)> {
    let Some(root) = parse_header(moov, 0) else { return Vec::new() };
    let mut out = Vec::new();
    for (ts, te, kind) in children(moov, root.header as usize, moov.len()) {
        if &kind != b"trak" {
            continue;
        }
        let Some((ms, me)) = child(moov, ts, te, b"mdia") else { continue };
        // hdlr: version and flags, pre_defined, then the handler type.
        let video = child(moov, ms, me, b"hdlr").and_then(|(hs, _)| moov.get(hs + 8..hs + 12)) == Some(b"vide");
        let Some((ns, ne)) = child(moov, ms, me, b"minf") else { continue };
        let Some((ss, se)) = child(moov, ns, ne, b"stbl") else { continue };
        out.push((ss, se, video));
    }
    out
}

/// Where the first video sample starts in the original file, and its size.
fn first_video_sample(moov: &[u8]) -> Option<(u64, u64)> {
    let (ss, se, _) = sample_tables(moov).into_iter().find(|t| t.2)?;
    let offset = if let Some((cs, _)) = child(moov, ss, se, b"stco") {
        (be32(moov, cs + 4)? > 0).then_some(())?;
        be32(moov, cs + 8)?
    } else {
        let (cs, _) = child(moov, ss, se, b"co64")?;
        (be32(moov, cs + 4)? > 0).then_some(())?;
        be64(moov, cs + 8)?
    };
    let (zs, _) = child(moov, ss, se, b"stsz")?;
    let uniform = be32(moov, zs + 4)?;
    let size = if uniform > 0 { uniform } else { be32(moov, zs + 12)? };
    Some((offset, size))
}

/// Every chunk offset in every track, moved `shift` bytes later.
fn shift_chunk_offsets(moov: &mut [u8], shift: u64) -> Result<(), String> {
    for (ss, se, _) in sample_tables(moov) {
        for (cs, ce, kind) in children(moov, ss, se) {
            let wide = match &kind {
                b"stco" => false,
                b"co64" => true,
                _ => continue,
            };
            let count = be32(moov, cs + 4).ok_or("Bad video index")? as usize;
            let width = if wide { 8 } else { 4 };
            if cs + 8 + count * width > ce {
                return Err("Bad video index".into());
            }
            for i in 0..count {
                let at = cs + 8 + i * width;
                if wide {
                    let v = be64(moov, at).unwrap() + shift;
                    moov[at..at + 8].copy_from_slice(&v.to_be_bytes());
                } else {
                    let v = u32::try_from(be32(moov, at).unwrap() + shift).map_err(|_| "Video too large to move")?;
                    moov[at..at + 4].copy_from_slice(&v.to_be_bytes());
                }
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn boxed(kind: &[u8; 4], payload: &[u8]) -> Vec<u8> {
        let mut b = ((payload.len() + 8) as u32).to_be_bytes().to_vec();
        b.extend_from_slice(kind);
        b.extend_from_slice(payload);
        b
    }

    fn full(kind: &[u8; 4], body: &[u8]) -> Vec<u8> {
        let mut p = vec![0, 0, 0, 0];
        p.extend_from_slice(body);
        boxed(kind, &p)
    }

    /// A moov with one track of `handler`, its chunk offsets and a uniform sample size.
    fn moov(tracks: &[(&[u8; 4], Vec<u32>, u32)]) -> Vec<u8> {
        let mut traks = Vec::new();
        for (handler, offsets, size) in tracks {
            let mut stco = (offsets.len() as u32).to_be_bytes().to_vec();
            for o in offsets {
                stco.extend_from_slice(&o.to_be_bytes());
            }
            let mut stsz = size.to_be_bytes().to_vec();
            stsz.extend_from_slice(&1u32.to_be_bytes());
            let stbl = boxed(b"stbl", &[full(b"stsz", &stsz), full(b"stco", &stco)].concat());
            let mut hdlr = vec![0, 0, 0, 0];
            hdlr.extend_from_slice(*handler);
            hdlr.extend_from_slice(&[0; 12]);
            let mdia = boxed(b"mdia", &[full(b"hdlr", &hdlr), boxed(b"minf", &stbl)].concat());
            traks.extend(boxed(b"trak", &mdia));
        }
        boxed(b"moov", &traks)
    }

    #[test]
    fn the_first_video_sample_is_found_past_an_audio_track() {
        let m = moov(&[(b"soun", vec![40], 10), (b"vide", vec![100, 900], 300)]);
        assert_eq!(first_video_sample(&m), Some((100, 300)));
        assert_eq!(first_video_sample(&moov(&[(b"soun", vec![40], 10)])), None);
    }

    #[test]
    fn moving_the_index_forward_shifts_every_chunk_offset_by_its_length() {
        let ftyp = boxed(b"ftyp", b"isom\0\0\0\0");
        let data_at = ftyp.len() as u32 + 8;
        let mdat = boxed(b"mdat", &[7u8; 64]);
        let m = moov(&[(b"vide", vec![data_at, data_at + 32], 16), (b"soun", vec![data_at + 16], 8)]);
        let file = [ftyp.clone(), mdat.clone(), m.clone()].concat();
        let out = faststart(&file[..ftyp.len() + mdat.len()], ftyp.len() as u64, m.clone()).unwrap();
        let layout = top_level(&out);
        let kinds: Vec<_> = layout.iter().map(|b| b.kind).collect();
        assert_eq!(kinds, vec![*b"ftyp", *b"moov", *b"mdat"]);
        let moved = &out[layout[1].offset as usize..layout[2].offset as usize];
        let (offset, size) = first_video_sample(moved).unwrap();
        assert_eq!((offset, size), (data_at as u64 + m.len() as u64, 16));
        // The sample the moved index points at is the same bytes it pointed at before.
        assert_eq!(&out[offset as usize..(offset + size) as usize], &file[data_at as usize..(data_at + 16) as usize]);
    }

    struct File(Vec<u8>, std::cell::Cell<u64>);

    impl Source for File {
        async fn range(&self, start: u64, end: u64) -> Result<Fetched, String> {
            let end = end.min(self.0.len() as u64);
            self.1.set(self.1.get() + end.saturating_sub(start));
            Ok(Fetched { bytes: self.0.get(start as usize..end as usize).unwrap_or_default().to_vec(), total: Some(self.0.len() as u64) })
        }
    }

    /// A real video's poster: `VIDEO_POSTER_IN=a.mp4 VIDEO_POSTER_OUT=b.mp4 cargo test … -- --ignored`.
    #[tokio::test]
    #[ignore]
    async fn a_poster_from_a_local_file() {
        let input = std::fs::read(std::env::var("VIDEO_POSTER_IN").unwrap()).unwrap();
        let file = File(input, std::cell::Cell::new(0));
        let poster = poster_from(&file).await.unwrap();
        println!("read {} of {} bytes, wrote {} ({})", file.1.get(), file.0.len(), poster.bytes.len(), poster.ext);
        std::fs::write(std::env::var("VIDEO_POSTER_OUT").unwrap(), poster.bytes).unwrap();
    }

    #[test]
    fn a_box_that_claims_less_than_its_header_ends_the_walk() {
        let mut buf = boxed(b"ftyp", b"isom\0\0\0\0");
        buf.extend_from_slice(&[0, 0, 0, 4, b'm', b'd', b'a', b't']);
        assert_eq!(top_level(&buf).len(), 1);
        assert!(parse_header(&[0, 0, 0, 1, b'm', b'd', b'a', b't', 0, 0, 0, 0, 0, 0, 0, 9], 0).is_none());
    }
}
