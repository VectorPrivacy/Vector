//! Keeps vault keys inside closures.
//!
//! [`GuardedKey::with_key`](crate::crypto::GuardedKey::with_key) and its kin hand the key to a
//! closure and zero the stack it used, so no copy outlives the call. An owned read
//! (`to_keys()`) hands out a keypair whose every move leaves a copy behind, which nothing
//! wipes. That is invisible in review, so it is caught mechanically: walk the source trees and
//! fail on an owned read in shipping code.
//!
//! A read whose result must outlive the call (a NIP-46 client keypair the session owns) is
//! exempted per site with a `// owned-key: <why>` marker on the line or just above it.

use std::path::Path;

// Assembled at compile time so this file never contains the text it searches for.
const NEEDLE: &str = concat!(".to_", "keys()");
const MARKER: &str = "owned-key:";

/// Every unmarked owned read in shipping code under `src_root`, as `path:line` relative to
/// `crate_root`.
pub fn owned_key_reads(crate_root: &Path, src_root: &Path) -> Vec<String> {
    let mut offenders = Vec::new();
    let mut stack = vec![src_root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else { continue };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
                continue;
            }
            if path.extension().and_then(|e| e.to_str()) != Some("rs") {
                continue;
            }
            let Ok(src) = std::fs::read_to_string(&path) else { continue };
            let rel = path.strip_prefix(crate_root).unwrap_or(&path).to_string_lossy().replace('\\', "/");
            for n in unmarked_lines(&src) {
                offenders.push(format!("{rel}:{n}"));
            }
        }
    }
    offenders.sort();
    offenders
}

/// The 1-based lines of `src`'s shipping code that read a vault key out of its closure.
/// Everything from the test module onward is skipped, as is a file that is itself `#![cfg(test)]`.
fn unmarked_lines(src: &str) -> Vec<usize> {
    let lines: Vec<&str> = src.lines().collect();
    let test_file = lines
        .iter()
        .map(|l| l.trim())
        .find(|l| !l.is_empty() && !l.starts_with("//"))
        .is_some_and(|l| l == "#![cfg(test)]");
    if test_file {
        return Vec::new();
    }
    let cut = lines
        .windows(2)
        .position(|w| w[0].trim() == "#[cfg(test)]" && w[1].trim_start().trim_start_matches("pub ").starts_with("mod "))
        .unwrap_or(lines.len());
    (0..cut)
        .filter(|&i| {
            let line = lines[i];
            line.contains(NEEDLE)
                && !line.trim_start().starts_with("//")
                && !line.contains(MARKER)
                && !lines[..i].iter().rev().take(4).any(|p| p.contains(MARKER))
        })
        .map(|i| i + 1)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_marked_read_passes_and_an_unmarked_one_fails() {
        let read = concat!("let k = VAULT", ".to_", "keys();");
        let unmarked = format!("fn f() {{\n    {read}\n}}\n");
        let marked = format!("fn f() {{\n    // {MARKER} the session owns it.\n    {read}\n}}\n");
        let in_tests = format!("fn f() {{}}\n#[cfg(test)]\nmod tests {{\n    {read}\n}}\n");
        assert_eq!(unmarked_lines(&unmarked), vec![2]);
        assert!(unmarked_lines(&marked).is_empty());
        assert!(unmarked_lines(&in_tests).is_empty());
    }

    /// Every crate in the workspace, and the Tauri shell beside it, wherever this checkout has them.
    #[test]
    fn no_vault_key_leaves_its_closure_unmarked() {
        let core = Path::new(env!("CARGO_MANIFEST_DIR"));
        let mut roots = vec![core.join("../../src-tauri")];
        if let Ok(entries) = std::fs::read_dir(core.join("..")) {
            roots.extend(entries.flatten().map(|e| e.path()).filter(|p| p.join("Cargo.toml").is_file()));
        }
        let mut roots: Vec<_> = roots.iter().filter_map(|r| r.canonicalize().ok()).filter(|r| r.join("src").is_dir()).collect();
        roots.push(core.canonicalize().unwrap());
        roots.sort();
        roots.dedup();
        let offenders: Vec<String> = roots
            .iter()
            .flat_map(|root| {
                let name = root.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                owned_key_reads(root, &root.join("src")).into_iter().map(move |o| format!("{name}/{o}"))
            })
            .collect();
        assert!(
            offenders.is_empty(),
            "these take a vault key out of its closure, leaving copies nothing wipes: use \
             with_key / with_keys / read, or mark the site `// {MARKER} <why it must outlive the call>`:\n  {}",
            offenders.join("\n  ")
        );
    }
}
