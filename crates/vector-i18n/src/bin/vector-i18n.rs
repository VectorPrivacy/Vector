//! `vector-i18n status [locales]`: each language's coverage, and what is missing, obsolete or
//! broken. `vector-i18n new <tag> [locales]`: an empty language that mirrors en-GB.

use std::path::{Path, PathBuf};

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let default = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("locales");
    match args.first().map(String::as_str) {
        Some("status") => status(&args.get(1).map_or(default, PathBuf::from), args.iter().any(|a| a == "-v")),
        Some("new") if args.len() >= 2 => new(&args[1], &args.get(2).map_or(default, PathBuf::from)),
        _ => {
            eprintln!("usage: vector-i18n status [locales] [-v] | vector-i18n new <tag> [locales]");
            std::process::exit(2);
        }
    }
}

fn read(dir: &Path) -> Vec<(String, String)> {
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
    let mut tags: Vec<String> = std::fs::read_dir(root)
        .unwrap_or_else(|e| panic!("{}: {e}", root.display()))
        .flatten()
        .filter(|e| e.path().is_dir())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|t| t != vector_i18n::BUILT_IN)
        .collect();
    tags.sort();
    println!("{:<12}{:>9}{:>9}{:>10}{:>8}", "language", "coverage", "missing", "obsolete", "broken");
    let mut failed = false;
    for tag in tags {
        let r = vector_i18n::check(&read(&root.join(&tag)));
        println!("{tag:<12}{:>8}%{:>9}{:>10}{:>8}", r.coverage(), r.missing.len(), r.obsolete.len(), r.broken.len());
        failed |= !r.broken.is_empty();
        if verbose {
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
