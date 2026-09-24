//! Tracks inside one audio file: an album or a mix carried as chapters.
//!
//! Three sources: ID3's CHAP frames (MP3), a text cue sheet in a CUESHEET comment
//! (FLAC and Ogg rips), and FLAC's binary CUESHEET block (points only, no titles).

use serde::Serialize;

#[derive(Serialize, Debug, Clone, PartialEq)]
pub struct Chapter {
    pub start_ms: u32,
    /// None for the last chapter: it runs to the end of the file.
    pub end_ms: Option<u32>,
    pub title: String,
}

/// Sorted, deduplicated by start, each ending where the next begins. Fewer than two is
/// no album.
fn finish(mut chapters: Vec<Chapter>) -> Vec<Chapter> {
    chapters.sort_by_key(|c| c.start_ms);
    chapters.dedup_by_key(|c| c.start_ms);
    if chapters.len() < 2 {
        return Vec::new();
    }
    let starts: Vec<u32> = chapters.iter().map(|c| c.start_ms).collect();
    let last = chapters.len() - 1;
    for (i, c) in chapters.iter_mut().enumerate() {
        c.end_ms = if i < last { Some(starts[i + 1]) } else { None };
        if c.title.trim().is_empty() {
            c.title = format!("Track {}", i + 1);
        }
    }
    chapters
}

// ── ID3 CHAP ──

fn id3_text(body: &[u8]) -> String {
    let Some((&enc, text)) = body.split_first() else { return String::new() };
    let s = match enc {
        0 => text.iter().map(|&b| b as char).collect(),
        1 | 2 => {
            let (units, big) = match text {
                [0xFE, 0xFF, rest @ ..] => (rest, true),
                [0xFF, 0xFE, rest @ ..] => (rest, false),
                rest => (rest, enc == 2),
            };
            let u16s: Vec<u16> = units
                .chunks_exact(2)
                .map(|c| if big { u16::from_be_bytes([c[0], c[1]]) } else { u16::from_le_bytes([c[0], c[1]]) })
                .collect();
            String::from_utf16_lossy(&u16s)
        }
        _ => String::from_utf8_lossy(text).into_owned(),
    };
    s.trim_matches(char::from(0)).trim().to_string()
}

/// A CHAP frame's body: element id, start and end in ms, byte offsets, then sub-frames
/// (sized syncsafe under ID3v2.4, plainly under v2.3), of which TIT2 is the title.
pub fn parse_chap(body: &[u8], v24: bool) -> Option<Chapter> {
    let nul = body.iter().position(|&b| b == 0)?;
    let rest = body.get(nul + 1..)?;
    if rest.len() < 16 {
        return None;
    }
    let start = u32::from_be_bytes(rest[0..4].try_into().ok()?);
    let mut subs = &rest[16..];
    let mut title = String::new();
    while subs.len() >= 10 {
        let id = &subs[0..4];
        let raw = [subs[4], subs[5], subs[6], subs[7]];
        let size = if v24 {
            ((raw[0] as usize) << 21) | ((raw[1] as usize) << 14) | ((raw[2] as usize) << 7) | raw[3] as usize
        } else {
            u32::from_be_bytes(raw) as usize
        };
        let Some(frame) = subs.get(10..10 + size) else { break };
        if id == b"TIT2" {
            title = id3_text(frame);
        }
        subs = &subs[10 + size..];
    }
    Some(Chapter { start_ms: start, end_ms: None, title })
}

fn read_id3(path: &str) -> Vec<Chapter> {
    use lofty::config::ParseOptions;
    use lofty::file::AudioFile;
    use lofty::id3::v2::{Frame, Id3v2Version};
    let Ok(mut file) = std::fs::File::open(path) else { return Vec::new() };
    let Ok(mpeg) = lofty::mpeg::MpegFile::read_from(&mut file, ParseOptions::new()) else { return Vec::new() };
    let Some(tag) = mpeg.id3v2() else { return Vec::new() };
    let v24 = matches!(tag.original_version(), Id3v2Version::V4);
    let chapters = tag
        .into_iter()
        .filter(|f| f.id_str() == "CHAP")
        .filter_map(|f| match f { Frame::Binary(b) => parse_chap(&b.data, v24), _ => None })
        .collect();
    finish(chapters)
}

// ── cue sheets ──

/// `mm:ss:ff`, frames at 75 a second.
fn cue_time(s: &str) -> Option<u32> {
    let mut it = s.trim().split(':');
    let (m, sec, f) = (it.next()?.parse::<u32>().ok()?, it.next()?.parse::<u32>().ok()?, it.next()?.parse::<u32>().ok()?);
    Some(m * 60_000 + sec * 1000 + f * 1000 / 75)
}

fn unquote(s: &str) -> String {
    let s = s.trim();
    s.strip_prefix('"').and_then(|x| x.strip_suffix('"')).unwrap_or(s).to_string()
}

/// A text cue sheet's tracks: each TRACK's TITLE and its INDEX 01.
pub fn parse_cue(text: &str) -> Vec<Chapter> {
    let mut chapters = Vec::new();
    let mut title = String::new();
    let mut in_track = false;
    for line in text.lines() {
        let line = line.trim();
        let (word, rest) = line.split_once(char::is_whitespace).unwrap_or((line, ""));
        match word.to_ascii_uppercase().as_str() {
            "TRACK" => { in_track = true; title.clear(); }
            "TITLE" if in_track => title = unquote(rest),
            "INDEX" if in_track => {
                let (num, time) = rest.trim().split_once(char::is_whitespace).unwrap_or(("", ""));
                if num.trim() == "01" {
                    if let Some(ms) = cue_time(time) {
                        chapters.push(Chapter { start_ms: ms, end_ms: None, title: title.clone() });
                    }
                }
            }
            _ => {}
        }
    }
    finish(chapters)
}

fn read_cue_comment(tagged: &lofty::file::TaggedFile) -> Vec<Chapter> {
    use lofty::file::TaggedFileExt;
    use lofty::tag::TagType;
    for tag in tagged.tags() {
        if tag.tag_type() != TagType::VorbisComments {
            continue;
        }
        for item in tag.items() {
            if let lofty::tag::ItemValue::Text(text) = item.value() {
                if text.contains("TRACK") && text.contains("INDEX") {
                    let chapters = parse_cue(text);
                    if !chapters.is_empty() {
                        return chapters;
                    }
                }
            }
        }
    }
    Vec::new()
}

/// FLAC's binary CUESHEET block: points without titles.
fn read_flac_cues(path: &str) -> Vec<Chapter> {
    use symphonia::core::formats::FormatOptions;
    use symphonia::core::io::MediaSourceStream;
    use symphonia::core::meta::MetadataOptions;
    use symphonia::core::probe::Hint;
    let Ok(file) = std::fs::File::open(path) else { return Vec::new() };
    let mss = MediaSourceStream::new(Box::new(file), Default::default());
    let mut hint = Hint::new();
    hint.with_extension("flac");
    let Ok(probed) = symphonia::default::get_probe().format(&hint, mss, &FormatOptions::default(), &MetadataOptions::default()) else {
        return Vec::new();
    };
    let format = probed.format;
    let Some(rate) = format.default_track().and_then(|t| t.codec_params.sample_rate) else { return Vec::new() };
    let chapters = format
        .cues()
        .iter()
        .map(|c| Chapter { start_ms: (c.start_ts * 1000 / rate as u64) as u32, end_ms: None, title: String::new() })
        .collect();
    finish(chapters)
}

/// Every source a file might carry, most descriptive first; empty when it is one track.
pub fn read(path: &str, tagged: &lofty::file::TaggedFile) -> Vec<Chapter> {
    let lower = path.to_ascii_lowercase();
    if lower.ends_with(".mp3") {
        return read_id3(path);
    }
    let from_cue = read_cue_comment(tagged);
    if !from_cue.is_empty() {
        return from_cue;
    }
    if lower.ends_with(".flac") {
        return read_flac_cues(path);
    }
    Vec::new()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chap(id: &str, start: u32, end: u32, title: &str, v24: bool) -> Vec<u8> {
        let mut b = id.as_bytes().to_vec();
        b.push(0);
        for n in [start, end, u32::MAX, u32::MAX] { b.extend(n.to_be_bytes()); }
        let mut body = vec![3u8];
        body.extend(title.as_bytes());
        b.extend(b"TIT2");
        let n = body.len() as u32;
        if v24 {
            b.extend([((n >> 21) & 0x7f) as u8, ((n >> 14) & 0x7f) as u8, ((n >> 7) & 0x7f) as u8, (n & 0x7f) as u8]);
        } else {
            b.extend(n.to_be_bytes());
        }
        b.extend([0, 0]);
        b.extend(body);
        b
    }

    #[test]
    fn a_chap_frame_gives_its_start_and_title() {
        for v24 in [true, false] {
            let c = parse_chap(&chap("chp1", 247_410, 545_570, "The Way You Make Me Feel", v24), v24).unwrap();
            assert_eq!((c.start_ms, c.title.as_str()), (247_410, "The Way You Make Me Feel"));
        }
    }

    #[test]
    fn utf16_titles_decode() {
        let mut body = vec![1u8, 0xFF, 0xFE];
        for u in "Señor".encode_utf16() { body.extend(u.to_le_bytes()); }
        assert_eq!(id3_text(&body), "Señor");
    }

    #[test]
    fn chapters_end_where_the_next_begins() {
        let got = finish(vec![
            Chapter { start_ms: 5_000, end_ms: None, title: "Two".into() },
            Chapter { start_ms: 0, end_ms: None, title: "One".into() },
            Chapter { start_ms: 9_000, end_ms: None, title: String::new() },
        ]);
        let got: Vec<_> = got.iter().map(|c| (c.start_ms, c.end_ms, c.title.as_str())).collect();
        assert_eq!(got, vec![(0, Some(5_000), "One"), (5_000, Some(9_000), "Two"), (9_000, None, "Track 3")]);
    }

    #[test]
    fn one_chapter_is_no_album() {
        assert!(finish(vec![Chapter { start_ms: 0, end_ms: None, title: "Only".into() }]).is_empty());
    }

    #[test]
    fn a_cue_sheet_gives_titles_and_index_one() {
        let cue = r#"PERFORMER "Someone"
TITLE "The Album"
FILE "album.flac" WAVE
  TRACK 01 AUDIO
    TITLE "Opening"
    INDEX 01 00:00:00
  TRACK 02 AUDIO
    TITLE "Second Song"
    INDEX 00 03:58:50
    INDEX 01 04:00:37
"#;
        let got: Vec<_> = parse_cue(cue).iter().map(|c| (c.start_ms, c.title.clone())).collect();
        assert_eq!(got, vec![(0, "Opening".to_string()), (240_493, "Second Song".to_string())]);
    }
}

