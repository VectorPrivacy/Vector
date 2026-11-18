//! Reads the en-GB catalogue and writes `catalog.rs`: the built-in sources, every message id
//! by index, and `define_t!`, one typed function per message for each client to instantiate.

#[path = "src/scan.rs"]
#[allow(dead_code)]
mod scan;

use std::fmt::Write as _;
use std::path::Path;

fn main() {
    let dir = Path::new(&std::env::var("CARGO_MANIFEST_DIR").unwrap()).join("locales/en-GB");
    println!("cargo:rerun-if-changed={}", dir.display());
    let mut files: Vec<_> = std::fs::read_dir(&dir)
        .expect("locales/en-GB")
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|x| x == "ftl"))
        .collect();
    files.sort();

    let mut units = vec![];
    let mut errors = vec![];
    let mut sources = String::new();
    for path in &files {
        println!("cargo:rerun-if-changed={}", path.display());
        let name = path.file_name().unwrap().to_string_lossy().to_string();
        let text = std::fs::read_to_string(path).unwrap();
        let s = scan::scan(&name, &text);
        errors.extend(s.errors);
        units.extend(s.units);
        writeln!(sources, "    ({name:?}, include_str!({:?})),", path.display().to_string()).unwrap();
    }
    let mut seen = std::collections::HashMap::new();
    for u in &units {
        if u.id.chars().any(|c| c.is_ascii_uppercase()) {
            errors.push(format!("`{}` must be lower case", u.id));
        }
        if let Some(other) = seen.insert(scan::fn_name(&u.id), u.id.clone()) {
            errors.push(format!("`{}` and `{other}` make the same function name", u.id));
        }
    }
    if !errors.is_empty() {
        panic!("en-GB catalogue:\n  {}", errors.join("\n  "));
    }
    scan::resolve(&mut units);

    let mut out = String::new();
    writeln!(out, "/// The built-in en-GB catalogue, file by file.").unwrap();
    writeln!(out, "pub(crate) const EN_GB: &[(&str, &str)] = &[\n{sources}];").unwrap();
    writeln!(out, "/// Every message and attribute the code can ask for, by index.").unwrap();
    writeln!(out, "pub(crate) const IDS: &[&str] = &[").unwrap();
    for u in &units {
        writeln!(out, "    {:?},", u.id).unwrap();
    }
    writeln!(out, "];\n/// Which take no values, so each language formats them once.").unwrap();
    writeln!(out, "pub(crate) const FIXED: &[bool] = &[").unwrap();
    for u in &units {
        writeln!(out, "    {},", u.vars.is_empty()).unwrap();
    }
    writeln!(out, "];\n").unwrap();

    writeln!(out, "/// One function per en-GB message, returning `$ty` (`From<Arc<str>> + From<String>`).").unwrap();
    writeln!(out, "#[macro_export]\nmacro_rules! define_t {{ ($ty:ty) => {{").unwrap();
    for (i, u) in units.iter().enumerate() {
        let doc = u.text.replace('\n', " ");
        writeln!(out, "    #[doc = {:?}]", format!("`{}`: {doc}", u.id)).unwrap();
        if let Some(c) = &u.comment {
            writeln!(out, "    #[doc = \"\"]").unwrap();
            for line in c.lines() {
                writeln!(out, "    #[doc = {:?}]", line).unwrap();
            }
        }
        let name = scan::fn_name(&u.id);
        if u.vars.is_empty() {
            writeln!(out, "    #[inline] pub fn {name}() -> $ty {{ <$ty as ::core::convert::From<::std::sync::Arc<str>>>::from($crate::fixed({i})) }}").unwrap();
            continue;
        }
        let params: Vec<String> = u
            .vars
            .iter()
            .map(|(v, k)| {
                let p = scan::fn_name(v);
                match k {
                    scan::Kind::Any => format!("{p}: impl ::core::convert::Into<$crate::Value<'v>>"),
                    scan::Kind::Num => format!("{p}: impl ::core::convert::Into<$crate::Num>"),
                    scan::Kind::List => format!("{p}: &[impl ::core::convert::AsRef<str>]"),
                }
            })
            .collect();
        let sets: String = u
            .vars
            .iter()
            .map(|(v, k)| {
                let p = scan::fn_name(v);
                match k {
                    scan::Kind::Any => format!("a.value({v:?}, {p}); "),
                    scan::Kind::Num => format!("a.num({v:?}, {p}); "),
                    scan::Kind::List => format!("a.list({v:?}, {p}); "),
                }
            })
            .collect();
        let life = if u.vars.values().any(|k| *k == scan::Kind::Any) { "<'v>" } else { "" };
        writeln!(
            out,
            "    pub fn {name}{life}({}) -> $ty {{ let mut a = $crate::Args::with_capacity({}); {sets}<$ty as ::core::convert::From<::std::string::String>>::from($crate::format({i}, &a)) }}",
            params.join(", "),
            u.vars.len()
        )
        .unwrap();
    }
    writeln!(out, "}} }}").unwrap();
    std::fs::write(Path::new(&std::env::var("OUT_DIR").unwrap()).join("catalog.rs"), out).unwrap();
}
