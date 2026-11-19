//! `vector-i18n status [locales]`: each language's coverage, and what is missing, obsolete or
//! broken. `vector-i18n new <tag> [locales]`: an empty language that mirrors en-GB.
//! `vector-i18n pack <tag> [locales] [-o file]`: the language as the app downloads it.
//!
//! With the `sign` feature, for CI and the key's holder: `publish <out> [locales] [--previous
//! index.json]` builds every language and the signed index, `keygen <file>` makes a signing key,
//! and `blossom-auth <file>` authorises one upload.

use std::path::{Path, PathBuf};

#[cfg(feature = "sign")]
mod publish;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let default = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("locales");
    match args.first().map(String::as_str) {
        Some("status") => status(&args.get(1).map_or(default, PathBuf::from), args.iter().any(|a| a == "-v")),
        Some("new") if args.len() >= 2 => new(&args[1], &args.get(2).map_or(default, PathBuf::from)),
        Some("pack") if args.len() >= 2 => {
            let out = args.iter().position(|a| a == "-o").and_then(|i| args.get(i + 1)).map(PathBuf::from);
            let root = args.get(2).filter(|a| *a != "-o").map_or(default, PathBuf::from);
            pack(&args[1], &root, out.unwrap_or_else(|| PathBuf::from(format!("{}.vpack", args[1]))));
        }
        #[cfg(feature = "sign")]
        Some("publish") if args.len() >= 2 => {
            let previous = args.iter().position(|a| a == "--previous").and_then(|i| args.get(i + 1)).map(PathBuf::from);
            let root = args.get(2).filter(|a| !a.starts_with("--")).map_or(default, PathBuf::from);
            publish::publish(&root, Path::new(&args[1]), previous.as_deref());
        }
        #[cfg(feature = "sign")]
        Some("keygen") if args.len() >= 2 => publish::keygen(Path::new(&args[1])),
        #[cfg(feature = "sign")]
        Some("blossom-auth") if args.len() >= 2 => publish::blossom_auth(Path::new(&args[1])),
        _ => {
            eprintln!("usage: vector-i18n status [locales] [-v] | new <tag> [locales] | pack <tag> [locales] [-o file]");
            if cfg!(feature = "sign") {
                eprintln!("       vector-i18n publish <out> [locales] [--previous index.json] | keygen <file> | blossom-auth <file>");
            }
            std::process::exit(2);
        }
    }
}

pub(crate) fn read(dir: &Path) -> Vec<(String, String)> {
    let mut files: Vec<(String, String)> = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "ftl"))
        .map(|p| (p.file_name().unwrap().to_string_lossy().into_owned(), std::fs::read_to_string(&p).unwrap_or_default()))
        .collect();
    files.sort();
    files
}

fn status(root: &Path, verbose: bool) {
    let tags = tags(root);
    println!("{:<12}{:>9}{:>9}{:>10}{:>8}", "language", "coverage", "missing", "obsolete", "broken");
    let mut failed = false;
    for tag in tags {
        let r = vector_i18n::check(&read(&root.join(&tag)));
        // An English variant's missing messages are en-GB's, which already read right.
        let missing = if vector_i18n::inherits_english(&tag) { 0 } else { r.missing.len() };
        println!("{tag:<12}{:>8}%{missing:>9}{:>10}{:>8}", r.coverage_for(&tag), r.obsolete.len(), r.broken.len());
        failed |= !r.broken.is_empty();
        if verbose && !vector_i18n::inherits_english(&tag) {
            for id in &r.missing {
                println!("    missing   {id}");
            }
            for id in &r.obsolete {
                println!("    obsolete  {id}");
            }
        }
        for (id, why) in &r.broken {
            println!("    broken    {id}: {why}");
        }
    }
    if failed {
        std::process::exit(1);
    }
}

fn new(tag: &str, root: &Path) {
    let dir = root.join(tag);
    if dir.exists() {
        eprintln!("{} already exists", dir.display());
        std::process::exit(1);
    }
    std::fs::create_dir_all(&dir).unwrap();
    for (name, _) in read(&root.join(vector_i18n::BUILT_IN)) {
        let header = format!("### {tag}: mirrors en-GB/{name}. Translate a message by copying its id from English.\n");
        std::fs::write(dir.join(&name), header).unwrap();
    }
    println!("created {}", dir.display());
}

/// A language as it ships, or why it can't: a broken message keeps the whole pack back.
pub(crate) fn build(tag: &str, root: &Path) -> Result<(vector_i18n::Pack, Vec<u8>), Vec<String>> {
    let mut p = vector_i18n::Pack::from_dir(tag, &root.join(tag)).map_err(|e| vec![format!("{}: {e}", root.join(tag).display())])?;
    let broken = vector_i18n::check(&p.files).broken;
    if !broken.is_empty() {
        return Err(broken.into_iter().map(|(id, why)| format!("{id}: {why}")).collect());
    }
    // Comments are for translators. A blank line can sit inside a multiline value, so it stays.
    for (_, src) in &mut p.files {
        *src = src.lines().filter(|l| !l.starts_with('#')).map(|l| format!("{l}\n")).collect();
    }
    let bytes = p.to_bytes();
    Ok((p, bytes))
}

fn pack(tag: &str, root: &Path, out: PathBuf) {
    let (p, bytes) = build(tag, root).unwrap_or_else(|broken| {
        for b in broken {
            eprintln!("broken    {b}");
        }
        std::process::exit(1);
    });
    std::fs::write(&out, &bytes).unwrap_or_else(|e| panic!("{}: {e}", out.display()));
    println!("{} ({}): {}% translated, {} bytes, {}", p.name, p.tag, p.coverage, bytes.len(), out.display());
}

/// Every language folder beside en-GB, sorted.
pub(crate) fn tags(root: &Path) -> Vec<String> {
    let mut tags: Vec<String> = std::fs::read_dir(root)
        .unwrap_or_else(|e| panic!("{}: {e}", root.display()))
        .flatten()
        .filter(|e| e.path().is_dir())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|t| t != vector_i18n::BUILT_IN)
        .collect();
    tags.sort();
    tags
}
