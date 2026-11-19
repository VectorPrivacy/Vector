//! Text vector-core writes for people (errors, network status, moderation wording), said in the
//! active language. Core speaks English; each line it can produce is matched back to the format
//! string that made it, and the values it carried are passed to that line's message.
//!
//! A line nothing here knows (a library's error, text that is already translated) shows as it
//! came. `core_text_ratchet` flags new core text that needs an entry.

use std::borrow::Cow;
use std::collections::HashMap;
use std::sync::{LazyLock, Mutex};

use crate::t;

/// Re-says one line through `t::`, from the values its format string placed. `None` when a
/// value isn't what the message needs, such as a count that isn't a number.
type Say = fn(&[String]) -> Option<String>;

/// Every core format string a person can meet, exactly as written in vector-core.
static TABLE: &[(&str, Say)] = &include!("coretext_table.rs");

struct Pattern {
    /// The literal text around each value: one more than the values.
    lits: Vec<String>,
    say: Say,
}

/// Most literal text first, so a longer line wins over a shorter one it contains.
static PATTERNS: LazyLock<Vec<Pattern>> = LazyLock::new(|| {
    let mut v: Vec<Pattern> = TABLE.iter().map(|(f, say)| Pattern { lits: literals(f), say: *say }).collect();
    v.sort_by_key(|p| std::cmp::Reverse(p.lits.iter().map(|l| l.len()).sum::<usize>()));
    v
});

/// Lines already said, per language generation; toasts and lists repeat the same few.
static SAID: LazyLock<Mutex<(u64, HashMap<String, Option<String>>)>> = LazyLock::new(|| Mutex::new((0, HashMap::new())));
const SAID_MAX: usize = 512;

/// The text around each `{…}` of a format string, with `{{` and `}}` as braces.
fn literals(format: &str) -> Vec<String> {
    let mut lits = vec![String::new()];
    let mut rest = format;
    while let Some(i) = rest.find(['{', '}']) {
        lits.last_mut().unwrap().push_str(&rest[..i]);
        rest = &rest[i..];
        if rest.starts_with("{{") || rest.starts_with("}}") {
            lits.last_mut().unwrap().push_str(&rest[..1]);
            rest = &rest[2..];
        } else if let Some(end) = rest.strip_prefix('{').and_then(|r| r.find('}')) {
            lits.push(String::new());
            rest = &rest[end + 2..];
        } else {
            lits.last_mut().unwrap().push_str(&rest[..1]);
            rest = &rest[1..];
        }
    }
    lits.last_mut().unwrap().push_str(rest);
    lits
}

/// Where each value sits in `s`, filling each as little as the text after it allows.
fn fit<'a>(s: &'a str, lits: &[String]) -> Option<Vec<&'a str>> {
    let first = lits.first()?;
    let rest = s.strip_prefix(first.as_str())?;
    let mut caps = Vec::with_capacity(lits.len() - 1);
    fn go<'a>(s: &'a str, lits: &[String], caps: &mut Vec<&'a str>) -> bool {
        let Some((lit, more)) = lits.split_first() else { return s.is_empty() };
        if more.is_empty() {
            let Some(v) = s.strip_suffix(lit.as_str()) else { return false };
            caps.push(v);
            return true;
        }
        if lit.is_empty() {
            caps.push("");
            if go(s, more, caps) {
                return true;
            }
            caps.pop();
            return false;
        }
        let mut from = 0;
        while let Some(i) = s[from..].find(lit.as_str()) {
            let at = from + i;
            caps.push(&s[..at]);
            if go(&s[at + lit.len()..], more, caps) {
                return true;
            }
            caps.pop();
            from = at + s[at..].chars().next().map_or(1, char::len_utf8);
        }
        false
    }
    if lits.len() == 1 {
        return rest.is_empty().then_some(caps);
    }
    go(rest, &lits[1..], &mut caps).then_some(caps)
}

fn find(s: &str, depth: u8) -> Option<String> {
    for p in PATTERNS.iter() {
        let Some(caps) = fit(s, &p.lits) else { continue };
        // A value may be a core line itself ("…: {error}"); its spacing stays as it was.
        let vals: Vec<String> = caps
            .iter()
            .map(|c| {
                let t = c.trim();
                match (depth < 3 && !t.is_empty()).then(|| find(t, depth + 1)).flatten() {
                    Some(said) => c.replacen(t, &said, 1),
                    None => c.to_string(),
                }
            })
            .collect();
        if let Some(said) = (p.say)(&vals) {
            return Some(said);
        }
    }
    None
}

/// `text` in the active language when it is a line vector-core writes, else as it is.
pub fn say(text: &str) -> Cow<'_, str> {
    let generation = vector_i18n::generation();
    if let Ok(mut c) = SAID.lock() {
        if c.0 != generation || c.1.len() > SAID_MAX {
            *c = (generation, HashMap::new());
        }
        if let Some(hit) = c.1.get(text) {
            return hit.clone().map_or(Cow::Borrowed(text), Cow::Owned);
        }
    }
    let said = find(text.trim(), 0);
    if let Ok(mut c) = SAID.lock() {
        c.1.insert(text.to_string(), said.clone());
    }
    said.map_or(Cow::Borrowed(text), Cow::Owned)
}

/// The keys a network status or check carries its text under; ids and codes sit elsewhere.
pub const STATUS_KEYS: &[&str] = &["text", "label", "title", "message", "hint", "error"];

/// `v` with the strings under `keys`, at any depth, said in the active language.
pub fn say_json(mut v: serde_json::Value, keys: &[&str]) -> serde_json::Value {
    fn walk(v: &mut serde_json::Value, keys: &[&str]) {
        match v {
            serde_json::Value::Object(m) => {
                for (k, x) in m.iter_mut() {
                    match x {
                        serde_json::Value::String(s) if keys.contains(&k.as_str()) => {
                            if let Cow::Owned(said) = say(s) {
                                *s = said;
                            }
                        }
                        _ => walk(x, keys),
                    }
                }
            }
            serde_json::Value::Array(a) => a.iter_mut().for_each(|x| walk(x, keys)),
            _ => {}
        }
    }
    walk(&mut v, keys);
    v
}

/// A count, passed as a number so a translation can choose plural forms on it.
fn n(s: &str) -> Option<f64> {
    s.trim().parse().ok()
}

/// Core's own plural, "3 hits" or "1 word": the number, when that is all there is.
fn counted(s: &str) -> Option<f64> {
    let (count, word) = s.trim().split_once(' ')?;
    (!word.contains(' ')).then_some(())?;
    n(count)
}

/// A policy's thresholds: "3 hits in one message, then 5 hits".
fn trips(list: &str) -> Option<String> {
    let mut said: Option<String> = None;
    for item in list.split(", then ") { // i18n-exempt: core's English, read back
        let item = match counted(item) {
            Some(count) => t::core_preset_hits(count).to_string(),
            None => match find(item, 1) {
                Some(s) => s,
                None => return None,
            },
        };
        said = Some(match said {
            Some(first) => t::core_preset_then(&first, &item).to_string(),
            None => item,
        });
    }
    Some(t::core_preset_trips(&said?).to_string())
}

/// A policy rule's plain-language detail: what it catches, then when it trips.
pub fn say_rule(detail: &str) -> String {
    // i18n-exempt: core's English, read back
    let (base, trips) = match detail.split_once(" Trips at ") {
        Some((b, rest)) => (b, Some(format!("Trips at {rest}"))), // i18n-exempt: core's English, read back
        None => (detail, None),
    };
    let mut out = match base.strip_suffix(" matched against each message.").and_then(counted) { // i18n-exempt: core's English
        Some(count) => t::core_preset_expressions(count).to_string(),
        None => say(base).into_owned(),
    };
    if let Some(trips) = trips {
        out.push(' ');
        out.push_str(&say(&trips));
    }
    out
}

/// The moderation policy catalogue (presets, their settings and rules, and the rule kinds) in
/// the active language. Only display text is said: ids and the policy bytes stay as they came.
pub fn say_presets(mut cat: serde_json::Value) -> serde_json::Value {
    const KEYS: &[&str] = &["name", "description", "example", "caveat", "label", "hint", "input_label", "input_hint"];
    for p in cat["presets"].as_array_mut().into_iter().flatten() {
        for r in p["rules"].as_array_mut().into_iter().flatten() {
            if let Some(d) = r["detail"].as_str() {
                r["detail"] = say_rule(d).into();
            }
        }
    }
    say_json(cat, KEYS)
}

/// `clock::describe_span`'s "5 minutes".
fn span(s: &str) -> Option<String> {
    let (count, unit) = s.split_once(' ')?;
    let count = n(count)?;
    let said = match unit.trim_end_matches('s') {
        "second" => t::core_span_seconds(count),
        "minute" => t::core_span_minutes(count),
        "hour" => t::core_span_hours(count),
        "day" => t::core_span_days(count),
        _ => return None,
    };
    Some(said.to_string())
}

/// A server blaming this device's clock: host, how far off, which way, and what to do.
fn clock(a: &[String]) -> Option<String> {
    let span = span(&a[1])?;
    let said = match a[2].as_str() {
        "slow" => t::core_upload_clock_slow(&a[3], &a[0], &span),
        "fast" => t::core_upload_clock_fast(&a[3], &a[0], &span),
        _ => return None,
    };
    Some(said.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::{BTreeMap, HashSet};
    use std::path::Path;

    const ALLOWANCE: &str = "core_text_allowance.txt";

    /// A literal as the compiler reads it.
    fn unescape(raw: &str) -> String {
        let mut out = String::with_capacity(raw.len());
        let mut it = raw.chars().peekable();
        while let Some(c) = it.next() {
            if c != '\\' {
                out.push(c);
                continue;
            }
            match it.next() {
                Some('n') => out.push('\n'),
                Some('t') => out.push('\t'),
                Some('r') => out.push('\r'),
                Some('0') => out.push('\0'),
                Some('u') => {
                    let hex: String = it.by_ref().skip(1).take_while(|c| *c != '}').collect();
                    out.extend(u32::from_str_radix(&hex, 16).ok().and_then(char::from_u32));
                }
                Some('x') => {
                    let hex: String = it.by_ref().take(2).collect();
                    out.extend(u8::from_str_radix(&hex, 16).ok().map(char::from));
                }
                // A line continuation: the newline and the next line's indent go.
                Some('\n') => {
                    while it.peek().is_some_and(|c| c.is_whitespace()) {
                        it.next();
                    }
                }
                Some(other) => out.push(other),
                None => out.push('\\'),
            }
        }
        out
    }

    fn core_src() -> std::path::PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../vector-core/src")
    }

    fn is_sql(s: &str) -> bool {
        ["SELECT ", "INSERT ", "UPDATE ", "DELETE FROM", "CREATE ", "ALTER ", "PRAGMA ", " FROM ", " WHERE "].iter().any(|k| s.contains(k))
    }

    #[test]
    fn core_text_every_entry_is_still_in_core() {
        let mut all = HashSet::new();
        let mut stack = vec![core_src()];
        while let Some(d) = stack.pop() {
            for e in std::fs::read_dir(&d).into_iter().flatten().flatten() {
                let p = e.path();
                if p.is_dir() {
                    stack.push(p);
                } else if p.extension().is_some_and(|x| x == "rs") {
                    let text = std::fs::read_to_string(&p).unwrap_or_default().replace("\r\n", "\n");
                    all.extend(crate::i18n_audit::literals(&text).into_iter().map(|(_, l)| unescape(&l)));
                }
            }
        }
        let gone: Vec<&str> = TABLE.iter().map(|(f, _)| *f).filter(|f| !all.contains(*f)).collect();
        assert!(gone.is_empty(), "vector-core no longer writes these; update coretext_table.rs and core.ftl:\n  {}", gone.join("\n  "));
    }

    /// New text in vector-core that a person might see needs a line in `coretext_table.rs`; what
    /// only a developer reads (logs, internal errors) raises the file's allowance instead:
    /// `I18N_RATCHET=write cargo test --bin vector-gpui core_text_ratchet`.
    #[test]
    fn core_text_ratchet() {
        let known: HashSet<&str> = TABLE.iter().map(|(f, _)| *f).collect();
        let found: BTreeMap<String, Vec<(usize, String)>> = crate::i18n_audit::offenders(&core_src())
            .into_iter()
            .map(|(f, hits)| (f, hits.into_iter().filter(|(_, l)| !is_sql(l) && !known.contains(unescape(l).as_str())).collect::<Vec<_>>()))
            .filter(|(_, hits)| !hits.is_empty())
            .collect();
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("src").join(ALLOWANCE);
        if std::env::var("I18N_RATCHET").as_deref() == Ok("write") {
            let total: usize = found.values().map(Vec::len).sum();
            let mut text = format!("# vector-core text with no coretext entry ({total} in all): logs and internal errors. See coretext.rs.\n");
            for (f, hits) in &found {
                text.push_str(&format!("{f} {}\n", hits.len()));
            }
            std::fs::write(&path, text).unwrap();
            return;
        }
        let allowed: BTreeMap<String, usize> = std::fs::read_to_string(&path)
            .unwrap_or_default()
            .lines()
            .filter(|l| !l.starts_with('#'))
            .filter_map(|l| l.rsplit_once(' ').and_then(|(f, n)| Some((f.to_string(), n.parse().ok()?))))
            .collect();
        let mut problems = vec![];
        for (f, hits) in &found {
            let allow = allowed.get(f).copied().unwrap_or(0);
            if hits.len() > allow {
                let mut p = format!("vector-core/src/{f}: {} strings with no coretext entry, allowance {allow}. A person can see these?", hits.len());
                for (n, s) in hits.iter().rev().take(hits.len() - allow) {
                    p.push_str(&format!("\n    {f}:{n} {s:?}"));
                }
                problems.push(p);
            }
        }
        for (f, allow) in &allowed {
            let now = found.get(f).map_or(0, Vec::len);
            if now < *allow {
                problems.push(format!("vector-core/src/{f}: down to {now} from {allow}; lower the allowance (I18N_RATCHET=write)"));
            }
        }
        assert!(problems.is_empty(), "\n{}", problems.join("\n"));
    }

    #[test]
    fn core_text_matches_back_to_its_format() {
        assert_eq!(literals("{} is still connecting."), vec!["", " is still connecting."]);
        assert_eq!(literals("a {{b}} {c:.1} d"), vec!["a {b} ", " d"]);
        let lits = literals("{relay} didn't take the announcement ({})");
        assert_eq!(fit("wss://a.b didn't take the announcement (rate limited)", &lits), Some(vec!["wss://a.b", "rate limited"]));
        let lits = literals("Posted the same message as {} other member{}");
        assert_eq!(fit("Posted the same message as 3 other members", &lits), Some(vec!["3", "s"]));
        assert_eq!(fit("Posted the same message as 1 other member", &lits), Some(vec!["1", ""]));
        assert_eq!(fit("Something else", &lits), None);
    }

    #[test]
    fn core_text_says_known_lines_and_keeps_the_rest() {
        assert_eq!(say("Vector is still connecting."), t::core_network_vector_still_connecting().to_string());
        assert_eq!(say("Tor is still connecting."), t::core_network_still_connecting("Tor").to_string());
        assert_eq!(say("connection reset by peer"), "connection reset by peer");
        assert_eq!(say("Checking 1 relays"), t::core_move_checking_relays(1).to_string());
        assert_eq!(say("Checking 3 relays"), "Checking 3 relays");
        let clock = say("cdn.example says this device's clock is about 5 minutes slow. Vector is checking the time with your relays; try again in a moment.");
        assert_eq!(clock, t::core_upload_clock_slow(&*t::core_upload_vector_checking_time_relays(), "cdn.example", &*t::core_span_minutes(5)).to_string());
        let rule = "3 words on the list, matched as whole words unless you wrap them in *. Trips at 1 hit in one message, then 5 hits.";
        let said = say_rule(rule);
        assert_eq!(said, rule.replace("1 hit in one message", &t::core_preset_one_message(1)).to_string());
        assert_eq!(say_rule("2 expressions matched against each message."), t::core_preset_expressions(2).to_string());
        let nested = say("Upload failed: Upload cancelled");
        assert!(nested.ends_with(&*t::core_upload_cancelled()), "{nested}");
    }
}
