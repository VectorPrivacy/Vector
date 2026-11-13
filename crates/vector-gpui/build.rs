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
        let Some(emoji) = field(t, "emoji") else {
            continue;
        };
        let name = field(t, "name").unwrap_or("").to_lowercase();
        let display = field(t, "display").unwrap_or("");
        let shortcode = match field(t, "shortcode") {
            Some(s) => s.to_string(),
            None => {
                let s: String = display
                    .to_lowercase()
                    .chars()
                    .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
                    .collect();
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
        let freq: u8 = t
            .find("freq: ")
            .and_then(|i| {
                t[i + 6..]
                    .split(|c: char| !c.is_ascii_digit())
                    .next()?
                    .parse()
                    .ok()
            })
            .unwrap_or(99);
        writeln!(out, "    Emoji {{ emoji: {emoji:?}, name: {name:?}, display: {display:?}, shortcode: {shortcode:?}, freq: {freq} }},").unwrap();
    }
    out.push_str("];\n");
    let dest = std::path::Path::new(&std::env::var("OUT_DIR").unwrap()).join("emoji_data.rs");
    std::fs::write(dest, out).unwrap();
    kaomoji();
    if std::env::var_os("CARGO_FEATURE_VIDEO_STATIC").is_some() {
        link_ffmpeg_system_libs();
    }
}

/// ffmpeg-sys-next links FFmpeg's own static libraries but not what they need from the
/// system (libm, frameworks); the build's pkg-config files list those.
fn link_ffmpeg_system_libs() {
    println!("cargo:rerun-if-env-changed=FFMPEG_DIR");
    let dir = std::env::var("FFMPEG_DIR")
        .expect("video-static needs FFMPEG_DIR=src-tauri/native-deps/ffmpeg/<target> (scripts/build-ffmpeg.sh)");
    let target_os = std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    let mut seen = std::collections::HashSet::new();
    let pc_dir = std::path::Path::new(&dir).join("lib/pkgconfig");
    for entry in std::fs::read_dir(&pc_dir).expect("FFMPEG_DIR has no lib/pkgconfig").flatten() {
        let text = std::fs::read_to_string(entry.path()).unwrap_or_default();
        for line in text.lines().filter(|l| l.starts_with("Libs:") || l.starts_with("Libs.private:")) {
            let mut tokens = line.split_whitespace().skip(1);
            while let Some(t) = tokens.next() {
                let lib = if t == "-framework" {
                    tokens.next().map(|f| format!("framework={f}"))
                } else if let Some(l) = t.strip_prefix("-l").or_else(|| t.strip_suffix(".lib")) {
                    // MSVC keeps libm and atomics in its CRT.
                    let crt = target_os == "windows" && matches!(l, "m" | "atomic");
                    (!l.starts_with("av") && !l.starts_with("sw") && !crt).then(|| l.to_string())
                } else if t == "-pthread" && target_os == "linux" {
                    Some("pthread".into())
                } else {
                    None
                };
                if let Some(lib) = lib.filter(|l| seen.insert(l.clone())) {
                    println!("cargo:rustc-link-lib={lib}");
                }
            }
        }
    }
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
    let src = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../src/components/lib/kaomoji.js"
    );
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
            writeln!(
                out,
                "        Kaomoji {{ face: {:?}, name: {:?}, tags: {:?} }},",
                l[0], l[1], l[2]
            )
            .unwrap();
        }
    }
    if open {
        out.push_str("    ] },\n");
    }
    out.push_str("];\n");
    let dest = std::path::Path::new(&std::env::var("OUT_DIR").unwrap()).join("kaomoji_data.rs");
    std::fs::write(dest, out).unwrap();
}
