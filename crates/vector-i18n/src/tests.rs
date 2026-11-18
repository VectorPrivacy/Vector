use super::*;

/// The active language is process-wide; tests that switch it take turns.
static SWITCH: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn pack(tag: &str, src: &str) -> Pack {
    Pack {
        tag: tag.into(),
        name: tag.into(),
        rtl: false,
        files: vec![("chat.ftl".into(), src.into())],
        data: LocaleData::default(),
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
    set_language(BUILT_IN);
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
    assert_eq!(langs[0], Language { tag: BUILT_IN.into(), name: "English (UK)".into(), coverage: 100 });
}
