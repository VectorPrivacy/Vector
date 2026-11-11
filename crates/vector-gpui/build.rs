//! The emoji and kaomoji tables, generated from the Tauri client's own datasets
//! (`src/js/emoji.js`, `src/components/lib/kaomoji.js`), so both clients list, name and rank the same.

use std::fmt::Write;

fn field<'a>(line: &'a str, key: &str) -> Option<&'a str> {
    let start = line.find(&format!("{key}: '"))? + key.len() + 3;
    let len = line[start..].find('\'')?;
    Some(&line[start..start + len])
}

fn main() {
    let src = concat!(env!("CARGO_MANIFEST_DIR"), "/../../src/js/emoji.js");
    println!("cargo:rerun-if-changed={src}");
    let js = std::fs::read_to_string(src).expect("emoji dataset");
    let mut out = String::from("pub static EMOJIS: &[Emoji] = &[\n");
    for line in js.lines() {
        let t = line.trim();
        if t.starts_with("// Apply") {
            break;
        }
        let Some(emoji) = field(t, "emoji") else { continue };
        let name = field(t, "name").unwrap_or("").to_lowercase();
        let display = field(t, "display").unwrap_or("");
        let shortcode = match field(t, "shortcode") {
            Some(s) => s.to_string(),
            None => {
                let s: String = display.to_lowercase().chars().map(|c| if c.is_ascii_alphanumeric() { c } else { '_' }).collect();
                let mut collapsed = String::new();
                for c in s.chars() {
                    if c == '_' && collapsed.ends_with('_') {
                        continue;
                    }
                    collapsed.push(c);
                }
                collapsed.trim_matches('_').to_string()
            }
        };
        let freq: u8 = t.find("freq: ").and_then(|i| t[i + 6..].split(|c: char| !c.is_ascii_digit()).next()?.parse().ok()).unwrap_or(99);
        writeln!(out, "    Emoji {{ emoji: {emoji:?}, name: {name:?}, display: {display:?}, shortcode: {shortcode:?}, freq: {freq} }},").unwrap();
    }
    out.push_str("];\n");
    let dest = std::path::Path::new(&std::env::var("OUT_DIR").unwrap()).join("emoji_data.rs");
    std::fs::write(dest, out).unwrap();
    kaomoji();
}

/// The JS string literals on a line, unescaped.
fn literals(line: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut chars = line.chars();
    while let Some(c) = chars.next() {
        if c != '\'' && c != '"' {
            continue;
        }
        let mut s = String::new();
        while let Some(d) = chars.next() {
            match d {
                '\\' => s.extend(chars.next()),
                d if d == c => break,
                d => s.push(d),
            }
        }
        out.push(s);
    }
    out
}

fn kaomoji() {
    let src = concat!(env!("CARGO_MANIFEST_DIR"), "/../../src/components/lib/kaomoji.js");
    println!("cargo:rerun-if-changed={src}");
    let js = std::fs::read_to_string(src).expect("kaomoji dataset");
    let mut out = String::from("pub static KAOMOJI: &[Category] = &[\n");
    let mut open = false;
    for line in js.lines() {
        let t = line.trim();
        if t.starts_with("{ name:") {
            if open {
                out.push_str("    ] },\n");
            }
            let name = &literals(t)[0];
            writeln!(out, "    Category {{ name: {name:?}, items: &[").unwrap();
            open = true;
        } else if open && t.starts_with('[') {
            let l = literals(t);
            writeln!(out, "        Kaomoji {{ face: {:?}, name: {:?}, tags: {:?} }},", l[0], l[1], l[2]).unwrap();
        }
    }
    if open {
        out.push_str("    ] },\n");
    }
    out.push_str("];\n");
    let dest = std::path::Path::new(&std::env::var("OUT_DIR").unwrap()).join("kaomoji_data.rs");
    std::fs::write(dest, out).unwrap();
}
