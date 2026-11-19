use super::*;

/// The active language is process-wide; tests that switch it take turns.
static SWITCH: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn pack(tag: &str, src: &str) -> Pack {
    Pack {
        tag: tag.into(),
        name: tag.into(),
        english_name: String::new(),
        rtl: false,
        files: vec![("chat.ftl".into(), src.into())],
        data: LocaleData::default(),
        emoji: String::new(),
        english: english_revision(),
        coverage: 0,
    }
}

#[test]
fn english_is_built_in() {
    let _g = SWITCH.lock().unwrap_or_else(|e| e.into_inner());
    set_language(BUILT_IN);
    assert_eq!(&*t::day_today(), "Today");
    assert_eq!(&*t::typing_group(2, "Alice", "Bob", ""), "Alice and Bob are typing...");
    assert_eq!(&*t::typing_group(7, "", "", ""), "Several people are typing...");
    assert_eq!(&*t::typing_strip(1, "Ann", "", ""), "<b>Ann</b> is typing");
}

#[test]
fn a_missing_or_broken_translation_falls_back_alone() {
    let _g = SWITCH.lock().unwrap_or_else(|e| e.into_inner());
    install(pack(
        "xx",
        "day-today = Hoy\n\
         typing-dm = { $who } escribe\n\
         typing-group = { $count -> \n  [one] { $first } escribe...\n *[other] Varios escriben...\n}\n\
         broken = { oops\n",
    ));
    assert!(set_language("xx"));
    assert_eq!(&*t::day_today(), "Hoy");
    assert_eq!(&*t::day_yesterday(), "Yesterday", "missing: English");
    assert_eq!(&*t::typing_dm(), "Typing...", "a value English doesn't pass: English");
    assert_eq!(&*t::typing_group(5, "", "", ""), "Varios escriben...");
    set_language(BUILT_IN);
}

#[test]
fn a_regional_pack_falls_back_to_its_parent_then_english() {
    let _g = SWITCH.lock().unwrap_or_else(|e| e.into_inner());
    install(pack("yy", "day-today = Parent\nday-yesterday = Parent yesterday\n"));
    install(pack("yy-ZZ", "day-today = Region\n"));
    assert!(set_language("yy-ZZ"));
    assert_eq!(&*t::day_today(), "Region");
    assert_eq!(&*t::day_yesterday(), "Parent yesterday");
    assert_eq!(&*t::typing_dm(), "Typing...");
    assert!(!set_language("nope"));
    assert_eq!(language(), BUILT_IN);
    assert_eq!(&*t::day_today(), "Today");
}

#[test]
fn plural_forms_come_from_the_language() {
    let _g = SWITCH.lock().unwrap_or_else(|e| e.into_inner());
    install(pack("pl", "typing-group = { $count ->\n  [one] jeden\n  [few] kilka\n *[many] wiele\n}\n"));
    set_language("pl");
    // Exact keys aside, Polish picks one, few or many by its own rules.
    assert_eq!(&*t::typing_group(1, "", "", ""), "jeden");
    assert_eq!(&*t::typing_group(3, "", "", ""), "kilka");
    assert_eq!(&*t::typing_group(5, "", "", ""), "wiele");
    assert_eq!(&*t::typing_group(22, "", "", ""), "kilka");
    set_language(BUILT_IN);
}

#[test]
fn numbers_read_in_the_language() {
    let _g = SWITCH.lock().unwrap_or_else(|e| e.into_inner());
    let mut p = pack("de", "typing-group = { NUMBER($count) } tippen, { BYTES(1536) }, { DURATION(65) }, { $count }\ntyping-strip = { NUMBER($count, useGrouping: \"false\") }\n");
    p.data = LocaleData { decimal: ",".into(), group: ".".into(), ..Default::default() };
    install(p);
    set_language("de");
    assert_eq!(&*t::typing_group(12345, "", "", ""), "12.345 tippen, 1,5 KB, 01:05, 12.345");
    assert_eq!(&*t::typing_strip(12345, "", "", ""), "12345");
    set_language(BUILT_IN);
}

#[test]
fn values_that_read_right_to_left_keep_their_isolation() {
    let _g = SWITCH.lock().unwrap_or_else(|e| e.into_inner());
    set_language(BUILT_IN);
    assert_eq!(&*t::typing_group(1, "Ann", "", ""), "Ann is typing...");
    assert_eq!(&*t::typing_group(1, "שלום", "", ""), "\u{2068}שלום\u{2069} is typing...");
}

#[test]
fn pseudo_locales_mark_every_message() {
    let _g = SWITCH.lock().unwrap_or_else(|e| e.into_inner());
    set_language(PSEUDO_ACCENTED);
    assert_eq!(&*t::day_today(), "[Ŧǿḓȧẏ··]");
    assert_eq!(&*t::typing_strip(1, "Ann", "", ""), "[<b>Ann</b> īş ŧẏƥīƞɠ·······]");
    set_language(PSEUDO_IDS);
    assert_eq!(&*t::typing_dm(), "typing-dm");
    set_language(PSEUDO_MIRRORED);
    assert!(is_rtl());
    assert_eq!(&*t::day_today(), "Today");
    set_language(BUILT_IN);
    assert!(!is_rtl());
    assert_eq!(&*t::day_today(), "Today");
}

#[test]
fn every_message_with_values_tells_translators_what_they_are() {
    for (file, src) in EN_GB {
        for u in scan::scan(file, src).units {
            assert!(u.vars.is_empty() || u.comment.is_some(), "{file}:{}: `{}` takes values but has no comment", u.line, u.id);
        }
    }
}

#[test]
fn the_checker_reports_missing_obsolete_and_broken() {
    let r = check(&[(
        "chat.ftl".into(),
        "day-today = Hoy\n\
         typing-dm = { $who }\n\
         typing-strip = { $count -> \n *[other] <i>x</i>\n}\n\
         gone = Adiós\n"
            .into(),
    )]);
    assert_eq!(r.total, IDS.len());
    assert_eq!(r.translated, 1);
    assert_eq!(r.obsolete, vec!["gone".to_string()]);
    assert!(r.missing.contains(&"day-yesterday".to_string()));
    let broken: Vec<&str> = r.broken.iter().map(|(id, _)| id.as_str()).collect();
    assert_eq!(broken, vec!["typing-dm", "typing-strip"]);
    assert_eq!(r.coverage(), (100 / IDS.len()) as u32);
    assert_eq!(check(&EN_GB.iter().map(|(f, s)| (f.to_string(), s.to_string())).collect::<Vec<_>>()).coverage(), 100);
}

#[test]
fn lookups_work_from_any_thread() {
    let _g = SWITCH.lock().unwrap_or_else(|e| e.into_inner());
    let h = std::thread::spawn(|| t::day_today().to_string());
    let s = h.join().unwrap();
    assert!(s == "Today" || s.contains('Ŧ') || s == "day-today" || s == "Hoy" || s == "Region");
}

#[test]
fn the_system_locale_sets_englishs_calendar_and_the_clock() {
    let _g = SWITCH.lock().unwrap_or_else(|e| e.into_inner());
    let noon = Civil { year: 2026, month: 3, day: 4, weekday: 3, hour: 13, minute: 0, second: 0 };
    set_system_locale("en-US", None);
    assert_eq!(locale().dates.date(&noon, DateStyle::Numeric), "3/4/2026");
    assert_eq!(locale().dates.time(&noon, Clock::SHORT), "1:00\u{202f}PM");
    set_system_locale("en_GB.UTF-8", Some(true));
    assert_eq!(locale().dates.date(&noon, DateStyle::Numeric), "04/03/2026");
    assert_eq!(locale().dates.time(&noon, Clock::SHORT), "1:00\u{202f}pm");
    install(pack("de", ""));
    set_language("de");
    assert!(locale().dates.hour12, "the system's clock holds in every language");
    set_system_locale("", None);
    set_language(BUILT_IN);
    assert_eq!(locale().dates.time(&noon, Clock::SHORT), "13:00");
}

#[test]
fn a_pack_survives_its_trip_and_refuses_what_isnt_one() {
    let dir = std::env::temp_dir().join(format!("vector-i18n-pack-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("language.ftl"), "language-name = Español\n").unwrap();
    std::fs::write(dir.join("chat.ftl"), "day-today = Hoy\n").unwrap();
    std::fs::write(dir.join("locale.json"), r#"{"decimal": ",", "dates": {"week_start": 1, "hour12": false}}"#).unwrap();
    let p = Pack::from_dir("es", &dir).unwrap();
    std::fs::remove_dir_all(&dir).ok();
    assert_eq!(p.name, "Español");
    assert_eq!(p.data.decimal, ",");
    assert_eq!(p.data.dates.months[0], "January", "what locale.json leaves out stays en-GB's");
    assert!(p.coverage < 5);
    assert_eq!(p.english, english_revision());

    let bytes = p.to_bytes();
    let back = Pack::from_bytes(&bytes).unwrap();
    assert_eq!((back.tag.as_str(), back.name.as_str(), back.coverage), ("es", "Español", p.coverage));
    assert_eq!(back.files, p.files);
    assert_eq!(back.data, p.data);

    assert!(Pack::from_bytes(b"not deflate at all").is_err());
    let mut english = back.clone();
    english.tag = BUILT_IN.into();
    assert!(Pack::from_bytes(&english.to_bytes()).is_err(), "a pack can't stand in for the built-in English");
    english.tag = "../es".into();
    assert!(Pack::from_bytes(&english.to_bytes()).is_err());
}

#[test]
fn the_built_in_english_names_itself() {
    let _g = SWITCH.lock().unwrap_or_else(|e| e.into_inner());
    let langs = languages();
    assert_eq!(
        langs[0],
        Language { tag: BUILT_IN.into(), name: "English (UK)".into(), english_name: "English (UK)".into(), coverage: 100 }
    );
}

#[test]
fn a_right_to_left_sentence_opening_in_latin_still_reads_right_to_left() {
    let _g = SWITCH.lock().unwrap_or_else(|e| e.into_inner());
    let mut p = pack(
        "he",
        "day-today = Vector \u{5d4}\u{5d9}\u{5d5}\u{5dd}\n\
         day-yesterday = \u{5d0}\u{5ea}\u{5de}\u{5d5}\u{5dc}\n\
         typing-strip = <b>{ $first }</b> \u{5de}\u{5e7}\u{5dc}\u{5d9}\u{5d3}\n\
         typing-dm = Vector\n",
    );
    p.rtl = true;
    install(p);
    set_language("he");
    assert_eq!(&*t::day_today(), "\u{200F}Vector \u{5d4}\u{5d9}\u{5d5}\u{5dd}");
    assert_eq!(&*t::day_yesterday(), "\u{5d0}\u{5ea}\u{5de}\u{5d5}\u{5dc}", "already right to left");
    assert_eq!(
        &*t::typing_strip(1, "Ann", "", ""),
        "<b>\u{2068}Ann\u{2069}</b> \u{5de}\u{5e7}\u{5dc}\u{5d9}\u{5d3}",
        "an isolated value doesn't decide the direction"
    );
    assert_eq!(&*t::typing_dm(), "Vector", "no right-to-left letters: left as written");
    assert_eq!(&*t::hidden_blocked(), "Blocked message", "English keeps its own direction");
    set_language(BUILT_IN);
}

#[test]
fn an_index_lists_only_what_this_build_can_install() {
    let entry = |tag: &str, format: u32, sha: &str| index::Entry {
        tag: tag.into(),
        name: tag.into(),
        english_name: String::new(),
        rtl: false,
        coverage: 90,
        format,
        size: 10,
        sha256: sha.into(),
        english: String::new(),
    };
    let good = "ab".repeat(32);
    let i = index::Index {
        format: index::INDEX_FORMAT,
        packs: vec![
            entry("es", PACK_FORMAT, &good),
            entry("de", PACK_FORMAT + 1, &good),
            entry("en-GB", PACK_FORMAT, &good),
            entry("../fr", PACK_FORMAT, &good),
            entry("ja", PACK_FORMAT, "AB"),
        ],
        next: None,
    };
    let back = index::Index::parse(&i.to_json()).unwrap();
    assert_eq!(back.usable().map(|e| e.tag.as_str()).collect::<Vec<_>>(), ["es"]);
    assert!(back.entry("de").is_none(), "a newer pack format waits for a newer app");
    let newer = index::Index { format: index::INDEX_FORMAT + 1, ..back };
    assert!(index::Index::parse(&newer.to_json()).is_err());
}

#[test]
fn an_english_variant_counts_what_it_inherits() {
    let r = check(&[("chat.ftl".into(), "day-today = Today\n".into())]);
    assert!(r.coverage() < 5);
    assert_eq!(r.coverage_for("en-US"), 100);
    assert_eq!(r.coverage_for("es"), r.coverage());
    let us = OFFERED.iter().find(|o| o.tag == "en-US").expect("the repository's en-US is offered");
    assert_eq!((us.name, us.english_name, us.coverage), ("English (US)", "English (US)", 100));
}

fn seed(tag: &str) -> LocaleData {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("seeds").join(format!("{tag}.json"));
    serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
}

#[test]
fn every_seed_reads_and_uses_only_letters_the_formatter_knows() {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("seeds");
    let mut seen = 0;
    for e in std::fs::read_dir(&dir).unwrap().flatten() {
        let tag = e.path().file_stem().unwrap().to_string_lossy().into_owned();
        let d = seed(&tag);
        for p in d.dates.patterns.iter().chain(&d.dates.times) {
            let mut quoted = false;
            for ch in p.chars() {
                quoted ^= ch == '\'';
                assert!(quoted || !ch.is_ascii_alphabetic() || "yMLdEchHKkmsa".contains(ch), "{tag}: {p:?} uses {ch:?}");
            }
        }
        assert_eq!(d.digits.chars().count(), 10, "{tag}");
        seen += 1;
    }
    assert!(seen >= 9, "the launch languages have seeds");
}

#[test]
fn japanese_and_chinese_seeds_write_their_own_dates() {
    use crate::dates::{Civil, Clock, DateStyle};
    let at = Civil { year: 2026, month: 11, day: 8, weekday: 0, hour: 14, minute: 5, second: 9 };
    let ja = seed("ja");
    assert_eq!(ja.date(&at, DateStyle::Full), "2026年11月8日日曜日");
    assert_eq!(ja.date(&at, DateStyle::WeekdayDayMonth), "11月8日(日)");
    assert_eq!(ja.time(&at, Clock::SHORT), "14:05");
    let twelve = crate::dates::Dates { hour12: true, ..ja.dates.clone() };
    assert_eq!(twelve.time(&at, Clock::SHORT), "午後2:05");
    assert_eq!(ja.list(&["A", "B", "C"]), "A、B、C");
    let zh = seed("zh-Hans");
    assert_eq!(zh.date(&at, DateStyle::Medium), "2026年11月8日");
    assert_eq!(zh.date_time(&at, DateStyle::Weekday, Clock::SHORT), "周日 14:05");
    assert_eq!(zh.list(&["A", "B", "C"]), "A、B和C");
}

#[test]
fn a_pack_carries_its_emoji_names_to_its_regional_variants() {
    let _g = SWITCH.lock().unwrap_or_else(|e| e.into_inner());
    let mut p = pack("xq", "chat-search = Busca\n");
    p.emoji = "😀\tcara sonriente\tsonrisa|cara\n".into();
    let p = Pack::from_bytes(&p.to_bytes()).unwrap();
    assert_eq!(p.emoji, "😀\tcara sonriente\tsonrisa|cara\n");
    install(p);
    install(pack("xq-YY", "chat-search = Busca\n"));
    assert!(set_language("xq-YY"));
    let names = emoji_names().expect("the parent's names");
    assert_eq!(names.name("😀"), Some("cara sonriente"));
    assert_eq!(names.words("😀").len(), 3);
    set_language(BUILT_IN);
    assert!(emoji_names().is_none());
}
