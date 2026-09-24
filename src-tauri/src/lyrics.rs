//! Lyrics embedded in an audio file, as lines the player can follow.
//!
//! Two sources: the lyrics tag every format has (ID3 USLT, Vorbis LYRICS, MP4 ©lyr),
//! whose text is either plain or LRC (`[mm:ss.xx] line`, optionally with enhanced
//! `<mm:ss.xx>` word stamps), and ID3's own synchronised frame (SYLT).

use serde::Serialize;

#[derive(Serialize, Debug, Clone, PartialEq)]
pub struct Word {
    pub at_ms: u32,
    pub text: String,
}

#[derive(Serialize, Debug, Clone, PartialEq)]
pub struct Line {
    /// None for plain lyrics.
    pub at_ms: Option<u32>,
    /// Empty on a timed line: an instrumental break.
    pub text: String,
    /// Word timings, when the source has them.
    pub words: Vec<Word>,
}

#[derive(Serialize, Debug, Clone, PartialEq)]
pub struct Lyrics {
    pub synced: bool,
    pub lines: Vec<Line>,
}

/// `mm:ss`, `mm:ss.x`, `mm:ss.xx`, `mm:ss.xxx`, or `mm:ss:xx`; minutes may run past 59.
fn parse_stamp(s: &str) -> Option<u32> {
    let s = s.trim();
    let (min, rest) = s.split_once(':')?;
    let min: u32 = min.trim().parse().ok()?;
    let (sec, frac) = match rest.split_once(['.', ':']) {
        Some((sec, frac)) => (sec, frac),
        None => (rest, ""),
    };
    let sec: u32 = sec.trim().parse().ok()?;
    if sec > 59 || !frac.chars().all(|c| c.is_ascii_digit()) || frac.len() > 3 {
        return None;
    }
    // The fraction is a decimal: ".5" is 500 ms, ".05" is 50 ms.
    let frac_ms = match frac.len() {
        0 => 0,
        n => frac.parse::<u32>().ok()? * 10u32.pow(3 - n as u32),
    };
    Some(min * 60_000 + sec * 1000 + frac_ms)
}

/// The leading `[..]` stamps of an LRC line, and what follows them.
fn leading_stamps(line: &str) -> (Vec<u32>, &str) {
    let mut stamps = Vec::new();
    let mut rest = line.trim_start();
    while let Some(body) = rest.strip_prefix('[') {
        let Some(end) = body.find(']') else { break };
        match parse_stamp(&body[..end]) {
            Some(ms) => { stamps.push(ms); rest = &body[end + 1..]; }
            None => break,
        }
    }
    (stamps, rest)
}

/// Enhanced LRC: `<00:12.30>word <00:12.80>word`. Text before the first stamp belongs
/// to the line's own time.
fn split_words(text: &str, line_at: u32) -> (String, Vec<Word>) {
    if !text.contains('<') {
        return (text.trim().to_string(), Vec::new());
    }
    let mut words = Vec::new();
    let mut plain = String::new();
    let mut at = line_at;
    let mut rest = text;
    let mut pending = String::new();
    loop {
        match rest.find('<') {
            Some(open) => {
                pending.push_str(&rest[..open]);
                let after = &rest[open + 1..];
                match after.find('>').and_then(|close| parse_stamp(&after[..close]).map(|ms| (close, ms))) {
                    Some((close, ms)) => {
                        if !pending.trim().is_empty() { words.push(Word { at_ms: at, text: pending.trim().to_string() }); }
                        plain.push_str(&pending);
                        pending.clear();
                        at = ms;
                        rest = &after[close + 1..];
                    }
                    None => { pending.push('<'); rest = after; }
                }
            }
            None => {
                pending.push_str(rest);
                if !pending.trim().is_empty() { words.push(Word { at_ms: at, text: pending.trim().to_string() }); }
                plain.push_str(&pending);
                break;
            }
        }
    }
    (plain.split_whitespace().collect::<Vec<_>>().join(" "), words)
}

/// Lyrics from the lyrics tag's text: LRC when most of its lines carry a stamp,
/// plain text otherwise.
pub fn parse_text(raw: &str) -> Option<Lyrics> {
    let raw = raw.trim_start_matches('\u{feff}');
    if raw.trim().is_empty() {
        return None;
    }
    let mut offset: i64 = 0;
    let mut timed: Vec<Line> = Vec::new();
    let mut content_lines = 0usize;
    let mut stamped_lines = 0usize;
    for line in raw.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        // ID tags: [ar:..], [ti:..], [offset:+250] ...
        if let Some(tag) = trimmed.strip_prefix('[').and_then(|t| t.strip_suffix(']')) {
            if let Some((key, value)) = tag.split_once(':') {
                if key.chars().all(|c| c.is_ascii_alphabetic()) {
                    if key.eq_ignore_ascii_case("offset") {
                        offset = value.trim().parse().unwrap_or(0);
                    }
                    continue;
                }
            }
        }
        content_lines += 1;
        let (stamps, text) = leading_stamps(trimmed);
        if stamps.is_empty() {
            continue;
        }
        stamped_lines += 1;
        for at in stamps {
            let (text, words) = split_words(text, at);
            timed.push(Line { at_ms: Some(at), text, words });
        }
    }
    if content_lines > 0 && stamped_lines * 2 > content_lines {
        // A positive offset shows the lyrics sooner.
        for line in &mut timed {
            let shift = |ms: u32| (ms as i64 - offset).max(0) as u32;
            line.at_ms = line.at_ms.map(shift);
            for w in &mut line.words { w.at_ms = shift(w.at_ms); }
        }
        timed.sort_by_key(|l| l.at_ms);
        return Some(Lyrics { synced: true, lines: timed });
    }
    let lines = raw
        .lines()
        .map(|l| Line { at_ms: None, text: l.trim().to_string(), words: Vec::new() })
        .collect::<Vec<_>>();
    // Trailing and leading blank lines carry nothing; blank lines inside are verse breaks.
    let first = lines.iter().position(|l| !l.text.is_empty())?;
    let last = lines.iter().rposition(|l| !l.text.is_empty())?;
    Some(Lyrics { synced: false, lines: lines[first..=last].to_vec() })
}

/// Lyrics from an ID3 SYLT frame's millisecond entries. An entry either is a whole line or
/// a syllable, with a leading newline starting each new line.
pub fn from_sylt(entries: &[(u32, String)]) -> Option<Lyrics> {
    if entries.is_empty() {
        return None;
    }
    let syllabic = entries.iter().skip(1).any(|(_, t)| t.starts_with('\n') || t.starts_with('\r'));
    let mut lines: Vec<Line> = Vec::new();
    for (at, text) in entries {
        if !syllabic {
            lines.push(Line { at_ms: Some(*at), text: text.trim().to_string(), words: Vec::new() });
            continue;
        }
        let starts_line = lines.is_empty() || text.starts_with('\n') || text.starts_with('\r');
        let piece = text.trim_start_matches(['\r', '\n']);
        if starts_line {
            lines.push(Line { at_ms: Some(*at), text: String::new(), words: Vec::new() });
        }
        let line = lines.last_mut()?;
        line.text.push_str(piece);
        if !piece.trim().is_empty() {
            line.words.push(Word { at_ms: *at, text: piece.to_string() });
        }
    }
    for line in &mut lines {
        line.text = line.text.trim().to_string();
    }
    lines.sort_by_key(|l| l.at_ms);
    Some(Lyrics { synced: true, lines })
}

/// Everything a file carries, synchronised lyrics first.
pub fn read(path: &str, tagged: &lofty::file::TaggedFile) -> Option<Lyrics> {
    use lofty::file::TaggedFileExt;
    use lofty::tag::ItemKey;
    if let Some(sylt) = read_sylt(path) {
        return Some(sylt);
    }
    let mut best: Option<Lyrics> = None;
    for tag in tagged.tags() {
        for key in [ItemKey::Lyrics, ItemKey::UnsyncLyrics] {
            let Some(text) = tag.get_string(key.clone()) else { continue };
            let Some(lyrics) = parse_text(text) else { continue };
            if lyrics.synced {
                return Some(lyrics);
            }
            best.get_or_insert(lyrics);
        }
    }
    best
}

/// ID3v2's SYLT, which lofty keeps as raw bytes; only millisecond stamps can be followed.
fn read_sylt(path: &str) -> Option<Lyrics> {
    use lofty::config::ParseOptions;
    use lofty::file::AudioFile;
    use lofty::id3::v2::{Frame, SynchronizedTextFrame, TimestampFormat};
    let lower = path.to_ascii_lowercase();
    if !lower.ends_with(".mp3") {
        return None;
    }
    let mut file = std::fs::File::open(path).ok()?;
    let mpeg = lofty::mpeg::MpegFile::read_from(&mut file, ParseOptions::new()).ok()?;
    let tag = mpeg.id3v2()?;
    for frame in tag {
        if frame.id_str() != "SYLT" {
            continue;
        }
        let Frame::Binary(bin) = frame else { continue };
        let Ok(sylt) = SynchronizedTextFrame::parse(&bin.data, frame.flags()) else { continue };
        if !matches!(sylt.timestamp_format, TimestampFormat::MS) {
            continue;
        }
        if let Some(lyrics) = from_sylt(&sylt.content) {
            return Some(lyrics);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stamps_read_every_common_shape() {
        assert_eq!(parse_stamp("00:12.34"), Some(12_340));
        assert_eq!(parse_stamp("01:02.5"), Some(62_500));
        assert_eq!(parse_stamp("01:02.050"), Some(62_050));
        assert_eq!(parse_stamp("01:02"), Some(62_000));
        assert_eq!(parse_stamp("01:02:34"), Some(62_340));
        assert_eq!(parse_stamp("75:00.00"), Some(4_500_000));
        assert_eq!(parse_stamp("ar:Someone"), None);
        assert_eq!(parse_stamp("00:61.00"), None);
    }

    #[test]
    fn lrc_lines_come_out_timed_and_ordered() {
        let raw = "[ti:Song]\n[ar:Artist]\n[00:05.00]First line\n[00:10.50]Second line\n[00:08.00]\n";
        let l = parse_text(raw).unwrap();
        assert!(l.synced);
        let got: Vec<_> = l.lines.iter().map(|x| (x.at_ms.unwrap(), x.text.as_str())).collect();
        assert_eq!(got, vec![(5_000, "First line"), (8_000, ""), (10_500, "Second line")]);
    }

    #[test]
    fn a_repeated_chorus_line_lands_at_each_of_its_stamps() {
        let l = parse_text("[00:10.00][01:10.00]Chorus\n[00:20.00]Verse").unwrap();
        let got: Vec<_> = l.lines.iter().map(|x| (x.at_ms.unwrap(), x.text.as_str())).collect();
        assert_eq!(got, vec![(10_000, "Chorus"), (20_000, "Verse"), (70_000, "Chorus")]);
    }

    #[test]
    fn a_positive_offset_brings_the_lyrics_sooner() {
        let l = parse_text("[offset:+500]\n[00:10.00]Line").unwrap();
        assert_eq!(l.lines[0].at_ms, Some(9_500));
    }

    #[test]
    fn enhanced_lrc_keeps_word_timings() {
        let l = parse_text("[00:12.00]<00:12.00>Hello <00:12.60>there <00:13.20>world").unwrap();
        let line = &l.lines[0];
        assert_eq!(line.text, "Hello there world");
        let words: Vec<_> = line.words.iter().map(|w| (w.at_ms, w.text.as_str())).collect();
        assert_eq!(words, vec![(12_000, "Hello"), (12_600, "there"), (13_200, "world")]);
    }

    #[test]
    fn plain_lyrics_stay_plain_with_their_verse_breaks() {
        let l = parse_text("\nFirst verse\nstill first\n\nSecond verse\n\n").unwrap();
        assert!(!l.synced);
        let got: Vec<_> = l.lines.iter().map(|x| x.text.as_str()).collect();
        assert_eq!(got, vec!["First verse", "still first", "", "Second verse"]);
    }

    #[test]
    fn a_stray_stamp_does_not_make_plain_lyrics_timed() {
        let l = parse_text("Line one\nLine two\n[00:01.00]Line three\nLine four").unwrap();
        assert!(!l.synced);
    }

    #[test]
    fn empty_text_is_no_lyrics() {
        assert_eq!(parse_text("  \n\n"), None);
    }

    #[test]
    fn sylt_syllables_build_lines_with_word_timings() {
        let entries = vec![
            (1_000, "Hel".to_string()), (1_300, "lo".to_string()),
            (2_000, "\nSe".to_string()), (2_400, "cond".to_string()),
        ];
        let l = from_sylt(&entries).unwrap();
        let got: Vec<_> = l.lines.iter().map(|x| (x.at_ms.unwrap(), x.text.as_str(), x.words.len())).collect();
        assert_eq!(got, vec![(1_000, "Hello", 2), (2_000, "Second", 2)]);
    }

    #[test]
    fn sylt_whole_lines_are_lines() {
        let entries = vec![(1_000, "One".to_string()), (3_000, "Two".to_string())];
        let l = from_sylt(&entries).unwrap();
        assert_eq!(l.lines.len(), 2);
        assert!(l.lines.iter().all(|x| x.words.is_empty()));
    }
}
