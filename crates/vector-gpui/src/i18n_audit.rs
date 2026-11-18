//! Hard-coded English in the UI, caught mechanically: every string literal that reads as prose
//! counts against its file's allowance in `i18n_allowance.txt`, which only goes down as text
//! moves into `crates/vector-i18n/locales/en-GB` behind `t::`. A literal that is not app text
//! (a symbol, `npub`, a protocol word) is exempted per site with `// i18n-exempt: <why>` on its
//! line or the one above.
//!
//! After converting strings, or when a file's count changes for a good reason:
//! `I18N_RATCHET=write cargo test --bin vector-gpui i18n_audit`.

use std::collections::BTreeMap;
use std::path::Path;

/// `(line, literal)` for each string literal in shipping code, comments and tests left out.
pub fn literals(src: &str) -> Vec<(usize, String)> {
    let shipping = match src.find("\n#[cfg(test)]\nmod tests") {
        Some(i) => &src[..i],
        None => src,
    };
    let b = shipping.as_bytes();
    let (mut i, mut line) = (0, 1);
    let mut out = vec![];
    while i < b.len() {
        match b[i] {
            b'\n' => {
                line += 1;
                i += 1;
            }
            b'/' if b.get(i + 1) == Some(&b'/') => {
                while i < b.len() && b[i] != b'\n' {
                    i += 1;
                }
            }
            b'/' if b.get(i + 1) == Some(&b'*') => {
                i += 2;
                while i + 1 < b.len() && !(b[i] == b'*' && b[i + 1] == b'/') {
                    line += (b[i] == b'\n') as usize;
                    i += 1;
                }
                i += 2;
            }
            b'\'' => {
                // A char literal ('"', '\'') or a lifetime ('a): skip either without misreading a quote.
                if b.get(i + 2) == Some(&b'\'') && b.get(i + 1) != Some(&b'\\') {
                    i += 3;
                } else if b.get(i + 1) == Some(&b'\\') {
                    let mut j = i + 2;
                    while j < b.len() && b[j] != b'\'' && j - i < 12 {
                        j += 1;
                    }
                    i = j + 1;
                } else {
                    i += 1;
                }
            }
            b'r' if (b.get(i + 1) == Some(&b'"') || b.get(i + 1) == Some(&b'#'))
                && (i == 0 || !(b[i - 1].is_ascii_alphanumeric() || b[i - 1] == b'_')) =>
            {
                let mut hashes = 0;
                let mut j = i + 1;
                while b.get(j) == Some(&b'#') {
                    hashes += 1;
                    j += 1;
                }
                if b.get(j) != Some(&b'"') {
                    i += 1;
                    continue;
                }
                let start = j + 1;
                let close: Vec<u8> = std::iter::once(b'"').chain(std::iter::repeat_n(b'#', hashes)).collect();
                let mut k = start;
                while k < b.len() && !b[k..].starts_with(&close) {
                    k += 1;
                }
                out.push((line, shipping[start..k.min(b.len())].to_string()));
                line += shipping[start..k.min(b.len())].matches('\n').count();
                i = k + close.len();
            }
            b'"' => {
                let start = i + 1;
                let mut k = start;
                while k < b.len() && b[k] != b'"' {
                    k += if b[k] == b'\\' { 2 } else { 1 };
                }
                let s = &shipping[start..k.min(b.len())];
                out.push((line, s.to_string()));
                line += s.matches('\n').count();
                i = k + 1;
            }
            _ => i += 1,
        }
    }
    out
}

/// Whether a literal reads as text a person sees, rather than a key, path or format.
pub fn is_prose(s: &str) -> bool {
    let words = s.split(|c: char| !c.is_ascii_alphabetic()).filter(|w| w.len() >= 2).count();
    if words == 0 {
        return false;
    }
    if s.starts_with("http") || s.starts_with("icons/") || s.starts_with("ui/") || s.contains("::") {
        return false;
    }
    if !s.contains(' ') && (s.contains('/') || s.contains('\\') || s.contains('.') && !s.ends_with('.')) {
        return false;
    }
    // Keys, ids, field names and kebab-case: no capital, no space.
    if !s.contains(' ') && !s.chars().any(|c| c.is_ascii_uppercase()) {
        return false;
    }
    // camelCase identifiers (and brands written like them).
    if !s.contains(' ') && s.as_bytes().windows(2).any(|w| w[0].is_ascii_lowercase() && w[1].is_ascii_uppercase()) {
        return false;
    }
    // SCREAMING_CASE constants and env vars.
    if !s.contains(' ') && s.chars().all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_') {
        return false;
    }
    true
}

/// Lines where a literal is for developers, not people: logs, panics and attributes.
fn developer_line(line: &str) -> bool {
    const DEV: &[&str] = &[
        "log_info!", "log_warn!", "log_debug!", "log_trace!", "log_error!", "log::", "eprintln!", "println!", "panic!", "unreachable!",
        "todo!", "assert", ".expect(", "#[", "debug!", "warn!(", "info!(", "trace!(", "error!(", "env::var", "dev_",
    ];
    DEV.iter().any(|d| line.contains(d))
}

/// Every prose literal under `src`, by file relative to it.
pub fn offenders(src: &Path) -> BTreeMap<String, Vec<(usize, String)>> {
    let mut out = BTreeMap::new();
    let mut stack = vec![src.to_path_buf()];
    while let Some(dir) = stack.pop() {
        for e in std::fs::read_dir(&dir).into_iter().flatten().flatten() {
            let p = e.path();
            if p.is_dir() {
                stack.push(p);
                continue;
            }
            if p.extension().is_none_or(|x| x != "rs") {
                continue;
            }
            let rel = p.strip_prefix(src).unwrap().to_string_lossy().replace('\\', "/");
            // Not app text: the developer bridge, code keywords to colour, file-format parsers,
            // test data, and the language-name table that packs will replace with CLDR names.
            if matches!(rel.as_str(), "devctl.rs" | "i18n_audit.rs" | "t.rs" | "highlight.rs" | "langnames.rs")
                || ["bin/", "model3d/", "testdata/"].iter().any(|d| rel.starts_with(d))
            {
                continue;
            }
            let text = std::fs::read_to_string(&p).unwrap_or_default();
            let lines: Vec<&str> = text.lines().collect();
            let hits: Vec<(usize, String)> = literals(&text)
                .into_iter()
                .filter(|(n, s)| {
                    let here = lines.get(n - 1).copied().unwrap_or("");
                    let above = if *n >= 2 { lines.get(n - 2).copied().unwrap_or("") } else { "" };
                    is_prose(s) && !developer_line(here) && !here.contains("i18n-exempt") && !above.contains("i18n-exempt")
                })
                .collect();
            if !hits.is_empty() {
                out.insert(rel, hits);
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const ALLOWANCE: &str = "i18n_allowance.txt";

    #[test]
    fn i18n_audit_no_file_gains_hard_coded_english() {
        let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let found = offenders(&src);
        let counts: BTreeMap<&str, usize> = found.iter().map(|(f, v)| (f.as_str(), v.len())).collect();
        let path = src.join(ALLOWANCE);
        if let Ok(out) = std::env::var("I18N_DUMP") {
            let mut text = String::new();
            for (f, hits) in &found {
                for (n, s) in hits {
                    text.push_str(&format!("{f}:{n} {s:?}\n"));
                }
            }
            std::fs::write(out, text).unwrap();
        }
        if std::env::var("I18N_RATCHET").as_deref() == Ok("write") {
            let total: usize = counts.values().sum();
            let mut text = format!("# Hard-coded English left per file ({total} in all). Only ever lowered; see i18n_audit.rs.\n");
            for (f, n) in &counts {
                text.push_str(&format!("{f} {n}\n"));
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
                let mut p = format!("{f}: {} hard-coded strings, allowance {allow}. Use t:: for new text:", hits.len());
                for (n, s) in hits.iter().rev().take(hits.len() - allow) {
                    p.push_str(&format!("\n    {f}:{n} {s:?}"));
                }
                problems.push(p);
            }
        }
        for (f, allow) in &allowed {
            let now = counts.get(f.as_str()).copied().unwrap_or(0);
            if now < *allow {
                problems.push(format!("{f}: down to {now} from {allow}; lower the allowance (I18N_RATCHET=write)"));
            }
        }
        assert!(problems.is_empty(), "\n{}", problems.join("\n"));
    }

    #[test]
    fn i18n_every_english_message_is_used() {
        let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let mut all = String::new();
        let mut stack = vec![src];
        while let Some(d) = stack.pop() {
            for e in std::fs::read_dir(&d).into_iter().flatten().flatten() {
                let p = e.path();
                if p.is_dir() {
                    stack.push(p);
                } else if p.extension().is_some_and(|x| x == "rs") {
                    all.push_str(&std::fs::read_to_string(&p).unwrap_or_default());
                }
            }
        }
        let unused: Vec<&str> = vector_i18n::ids().iter().copied().filter(|id| !all.contains(&format!("t::{}(", vector_i18n::function_name(id)))).collect();
        assert!(unused.is_empty(), "en-GB messages nothing calls: {unused:?}");
    }

    #[test]
    fn i18n_audit_reads_literals_like_the_compiler() {
        let src = "let a = \"Hello there\"; // \"not this\"\nlet c = '\"'; let d = r#\"Raw \"text\"\"#;\n/* \"nor this\" */ x(\"Save\");\n";
        let lits: Vec<String> = literals(src).into_iter().map(|(_, s)| s).collect();
        assert_eq!(lits, vec!["Hello there", "Raw \"text\"", "Save"]);
        assert!(is_prose("Save") && is_prose("Hello there") && is_prose("{} is typing..."));
        assert!(!is_prose("typing-strip") && !is_prose("icons/x.svg") && !is_prose("ws_list_width") && !is_prose("VECTOR_LANG") && !is_prose("{}"));
        assert!(!is_prose("ranSiteID") && is_prose("Save Changes"));
    }
}
