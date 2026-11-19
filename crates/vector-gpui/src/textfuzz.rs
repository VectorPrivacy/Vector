//! Text that arrives from other people goes through every parser here; none may panic on it.
//! `TEXTFUZZ_ITERS` runs longer than the suite's default.

use std::collections::HashSet;
use std::panic::{AssertUnwindSafe, catch_unwind};

use crate::inline::Look;

const TOKENS: &[&str] = &[
    "a", "Z", "0", "9", " ", "  ", "\n", "\n\n", "\r\n", "\t", "\u{0}", "*", "**", "_", "__", "~", "~~", "`", "```", "```rust\n",
    "||", "> ", ">", "<", "</", "<b>", "</b>", "<br>", "&amp;", "&#x", "&", "#", "# ", "## ", "- ", "* ", "1. ", "[", "]", "(", ")",
    "](", "!", "![", ":", "::", ":smile:", ":grin:", ":a", "@", "@everyone", "#{", "}", "{", "\\", "/", "//", "http://", "https://",
    "www.", ".com", "example.com/", "?q=", "#frag", "nostr:", "npub1", "nprofile1", "note1", "nevent1", "naddr1", "lnbc1",
    "vector://", "https://vectorapp.io/", "/invite/", "go.vectorapp.io/", "bitcoin:", "pivx:", "D", "1BvBMSEYstWetqTFn5Au4m4GFg7xJaNVN2",
    "qpzry9x8gf2tvdw0s3jn54khce6mua7l", "é", "e\u{301}", "ß", "İ", "東京", "で", "カフェ", "見つけました", "。", "、", "「", "」",
    "中文", "한국어", "😀", "👍🏽", "👨\u{200d}👩\u{200d}👧", "🇯🇵", "1\u{fe0f}\u{20e3}", "\u{fe0f}", "\u{200d}", "\u{200b}",
    "\u{200e}", "\u{200f}", "\u{202e}", "\u{2066}", "\u{2069}", "\u{feff}", "\u{2028}", "\u{fffd}", "שלום", "مرحبا", "١٢٣",
    "ไทย", "हिन्दी", "＃", "ｗ", "\u{1f3fb}", "\u{e000}", "\u{10ffff}", "(◕‿◕)", "¯\\_(ツ)_/¯", ":)", ":(", "<3", ":D", "->",
    "00:00", "12:34:56", "$", "%", "+", "=", "|", "^", "'", "\"", "https://vectorapp.io/profile/", "<t:", "<t:1700000000>",
    "<t:-9999999999999:R>", ":R>", ":f>", "@time ", "/time ", "in ", "at ", "on ", "next ", "last ", "this ", "tomorrow", "today",
    "jan", "Feb", "sept", "décembre", "monday", "fri", "weekend", "pm", "am", "noon", "midnight", "half an ", "a ", "an ",
    "hour", "hours", "2h", "30m", "1d", "and ", "3rd", "31", "2026", "/", "-", ".", ",",
    "https://www.amazon.com/dp/", "https://amazon.co.uk/x/dp/", "https://youtu.be/x?si=", "&utm_source=", "?ref=", "=",
    "https://www.google.com/search?q=", "https://www.ebay.com/itm/", "123456789",
    "[00:12.34]", "[99:99.999]", "[-1:00]", "<00:01.00>", "[ar:", "[offset:", "[offset:-99999999]", "TRACK 01 AUDIO\n",
    "INDEX 01 00:00:00\n", "INDEX 01 99:99:99\n", "TITLE \"", "PERFORMER \"", "FILE \"a.flac\" WAVE\n", "REM ",
    ".xdc", "app%20x.xdc", "%E6%9D%B1", "%", "+", "v0.5.2", "-preview.", "0.6.0-", "99999999999999999999", "<b>", "</b>",
    "<span class=\"", "&lt;", "&#x1F600;", "&#99999999;", "member.ban", "channel.", "(╯°□°)╯", "^_^",
];

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    fn below(&mut self, n: usize) -> usize {
        (self.next() % n.max(1) as u64) as usize
    }
    fn text(&mut self) -> String {
        let mut s = String::new();
        for _ in 0..self.below(40) {
            s.push_str(TOKENS[self.below(TOKENS.len())]);
        }
        s
    }
    /// A char boundary of `s`, or now and then one past its end.
    fn boundary(&mut self, s: &str) -> usize {
        let b: Vec<usize> = s.char_indices().map(|(i, _)| i).chain([s.len()]).collect();
        b[self.below(b.len())]
    }
}

fn look() -> Look {
    Look::new("Rubik", 15., 21., gpui::white()) // i18n-exempt: font name
}

/// Every parser over `s`; `r` picks positions and spans for the ones that take them.
fn all(s: &str, r: &mut Rng) {
    use crate::*;
    let revealed = HashSet::new();
    let tags = vec![("everyone".to_string(), None), ("mods".to_string(), Some(0xff0000))];
    let spans: Vec<vector_core::text_color::ColorSpan> = (0..r.below(4))
        .map(|_| vector_core::text_color::ColorSpan {
            from: r.below(s.chars().count() + 4) as u32,
            to: r.below(s.chars().count() + 4) as u32,
            effect: [vector_core::text_color::Effect::Solid, vector_core::text_color::Effect::Rainbow, vector_core::text_color::Effect::Gradient][r.below(3)],
            colors: (0..r.below(3)).map(|_| ["#ff0000", "zz", "", "#12", "#ABCDEF80"][r.below(5)].to_string()).collect(),
        })
        .collect();
    let mention = |n: &str| format!("@{}", &n[..n.len().min(4)]);
    let mask = |t: &str| t.contains("npub1").then(|| t.replace("npub1", "•••"));
    let ctx = markdown::Ctx {
        base: look(),
        revealed: &revealed,
        custom: None,
        now: 1_700_000_000.,
        mention: Some(&mention),
        tags: &tags,
        mask: Some(&mask),
        colors: &spans,
        go: None,
    };
    let _ = markdown::parse(s, &ctx);
    let _ = markdown::plain(s, &ctx);
    let _ = markdown::flatten(s);
    let _ = markdown::find_npubs(s);
    let _ = markdown::find_urls(s);
    let _ = markdown::shortcodes(s);
    let _ = embeds::refs(s);
    let _ = embeds::strip(s);
    let _ = embeds::tidy(s);
    let _ = embeds::for_preview(s);
    let _ = embeds::clip_id(s);
    let _ = embeds::preview_id(s);
    let _ = emoji::plain_text_symbols(s);
    let _ = emoji::only_emoji_count(s);
    let _ = emoji::in_text(s);
    let _ = emoji::search_scored(s, |_| 0.);
    let _ = emoji::search_kaomoji(s);
    let _ = emoji::twemoji(s);
    let _ = inline::has_rtl(s);
    let _ = inline::emoji_line(s, look(), 40., 0.);
    let _ = kit::untag(s);
    let _ = kit::runs(s);
    let _ = kit::reads_ltr(s);
    let _ = kit::around(s, "b");
    let _ = kit::escape(s);
    let _ = mentions::trigger(s, r.boundary(s), '@');
    let _ = mentions::resolve(s, &[("alice".into(), "npub1alice".into()), ("東京".into(), "npub1x".into())]);
    let _ = shortcodes::trigger(s, r.boundary(s));
    let _ = shortcodes::text_symbol(s, r.boundary(s));
    let _ = newchat::go_link(s);
    let _ = newchat::invite_key(s);
    let _ = newchat::invite_links(s);
    let _ = newchat::invite_card_keys(s);
    let _ = newchat::extract_npub(s);
    let _ = payaddr::detect(s);
    let _ = preview::plain(s);
    let _ = preview::atoms(s, r.below(80), &look());
    let _ = preview::reply_atoms(s, &look());
    let _ = paste_md::html_to_markdown(s, s);
    for lang in ["rust", "js", "python", "json", "html", "css", "sh", "", "zz"] {
        let _ = highlight::colored(s, lang);
        let _ = highlight::shown(s, lang);
    }
    let _ = textcolor::mark(s, &spans, &[]);
    let _ = textcolor::command(s);
    let _ = textcolor::command_to_markup(s);
    let _ = textcolor::composer_paint(s, r.below(2) == 0, &[]);
    let _ = textcolor::color_choices(s);
    let _ = textcolor::swatch(s);
    let _ = decorate::tokenize(s, &["alice".into(), "東京".into()]);
    let (a, b) = (r.boundary(s), r.boundary(s));
    let _ = marks::toggle(s, a.min(b)..a.max(b), ["**", "*", "~~", "`", "||"][r.below(5)]);
    let _ = cmdform::assemble(s, &[s.to_string(), String::new()]);
    let _ = audio::split_title(s);
    let _ = commsettings::relay_address(s, &["wss://a.example".into()], None);
    let _ = commsettings::voice_address(s, &[]);
    let _ = commsettings::short_npub(s);
    let _ = downgrade::display(s);
    let _ = data::find_bech_people(s);
    let _ = data::file_type(s);
    let _ = packs::refs(s);
    let _ = packs::strip(s);
    let _ = coretext::say(s);
    let _ = coretext::say_rule(s);
    let _ = textdiff::tokenize(s);
    let other = r.text();
    let ops = textdiff::diff_words(s, &other);
    let _ = textdiff::fold_unchanged(&ops, r.below(4), r.below(4));
    let _ = select::key_for(s);
    let _ = langnames::name(s);
    let _ = langnames::flag(s);
    let _ = roles::parse_hex(s);
    let _ = settings::host(s);
    let _ = filepreview::file_icon(s);
    let now = chrono::TimeZone::timestamp_opt(&chrono::Local, 1_700_000_000, 0).single().unwrap();
    let caret = r.boundary(s);
    let _ = texttime::duration(s);
    let _ = texttime::parse_full(s, now);
    let _ = texttime::suggest(s, now);
    let _ = texttime::codes(s);
    let _ = texttime::find(s, caret);
    let _ = texttime::picker(s, caret, now, None);
    let _ = texttime::localize(s, &[], 1_700_000_000.);
    let _ = untrack::clean(s);
    let _ = untrack::text(s);
    let _ = vector_media::lyrics::parse_text(s);
    let _ = vector_media::chapters::parse_cue(s);
    let _ = vector_media::chapters::parse_chap(s.as_bytes(), r.below(2) == 0);
    let _ = untrack::shown(s);
    let _ = newchat::invite_url(s);
    let _ = backend::gifs::original_url(s);
    let _ = backend::xdcurl::find(s);
    let _ = backend::xdcurl::file_name(s);
    let _ = backend::xdcurl::host(s);
    let _ = backend::updates::Version::parse(s);
    let _ = timeline::is_image_url(s);
    let _ = texttime::code_at(s);
    let _ = kit::rich(s);
    let _ = emoji::text_symbol_code(s);
    let _ = emoji::by_char(s);
    let _ = emoji::kaomoji(s);
    let _ = audit::family_of(s);
    let _ = audit::tone_of(s);
    let _ = uploaderr::humanize(s);
    let _ = nexus::tag_label(s);
    let _ = routes::tag_colors(s);
}

#[test]
fn no_text_panics_a_parser() {
    let iters: usize = std::env::var("TEXTFUZZ_ITERS").ok().and_then(|v| v.parse().ok()).unwrap_or(1500);
    let seed: u64 = std::env::var("TEXTFUZZ_SEED").ok().and_then(|v| v.parse().ok()).unwrap_or(0x9e37_79b9_7f4a_7c15);
    let mut r = Rng(seed);
    let mut failed = Vec::new();
    for _ in 0..iters {
        let s = r.text();
        let state = r.0;
        if catch_unwind(AssertUnwindSafe(|| all(&s, &mut Rng(state | 1)))).is_err() {
            failed.push(s);
            if failed.len() >= 5 {
                break;
            }
        }
    }
    assert!(failed.is_empty(), "inputs that panicked: {failed:#?}");
}

/// Deep nesting and long runs, on the stack a main thread gets.
#[test]
fn deep_and_long_text_parses() {
    let cases: Vec<String> = [
        ">", "> ", "[", "](", "*", "**", "_", "~~", "`", "(", "<", "<b>", "- ", "  ", "1. ", "|", "#{", "\n", "a", "😀", ":a:", "npub1",
        "https://", "**a", "||", "![", "&amp;", "\\", "#", "e\u{301}", "東京", "שלום ",
    ]
    .iter()
    .flat_map(|t| [t.repeat(20_000), format!("{}x", t.repeat(5_000))])
    .chain([format!("{}x{}", "> ".repeat(3_000), "\n".repeat(10)), "- ".repeat(2_000) + "\n" + &"  - x\n".repeat(2_000)])
    .collect();
    let slow = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let out = slow.clone();
    std::thread::Builder::new()
        .stack_size(8 << 20)
        .spawn(move || {
            for s in &cases {
                let t = std::time::Instant::now();
                all(s, &mut Rng(7));
                out.lock().unwrap().push((t.elapsed().as_millis(), s.chars().take(12).collect::<String>(), s.len()));
            }
        })
        .unwrap()
        .join()
        .expect("a parser panicked or overflowed on deep or long text");
    if std::env::var_os("TEXTFUZZ_TIMES").is_some() {
        let mut v = slow.lock().unwrap().clone();
        v.sort_by(|a, b| b.0.cmp(&a.0));
        for (ms, head, len) in v.iter().take(12) {
            eprintln!("{ms:>6} ms  {len:>7} bytes  {head:?}");
        }
    }
}
