//! The policy check behind [`crate::db::spawn_bound`], shared by every crate
//! that spawns per-account work.
//!
//! A bare `tokio::spawn` leaves its task resolving whoever is logged in when it
//! finally asks, which is how account A's work lands in account B's storage.
//! That mistake is invisible in review, so it is caught mechanically instead:
//! walk a source tree and fail on an unbound spawn.
//!
//! A task that genuinely owns no account state — a process-lifetime listener, a
//! socket drain, CPU work on bytes already in hand — is exempted per site with a
//! `// spawn-detached: <why>` marker on the line or just above it. Per site, not
//! per file: files hold both kinds, and exempting one wholesale hides the other.

use std::path::Path;

/// Every unbound `tokio::spawn` under `src_root`, as `path:line` relative to
/// `crate_root`. Empty means the tree is clean.
pub fn unbound_spawns(crate_root: &Path, src_root: &Path) -> Vec<String> {
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
            let rel = path
                .strip_prefix(crate_root)
                .unwrap_or(&path)
                .to_string_lossy()
                .replace('\\', "/");
            let Ok(src) = std::fs::read_to_string(&path) else { continue };
            for (line, n) in unbound_lines(&src) {
                let _ = line;
                offenders.push(format!("{rel}:{n}"));
            }
        }
    }
    offenders.sort();
    offenders
}

/// Whether one file's shipping code still spawns unbound. Used by the ratchet.
pub fn has_unbound_spawn(src: &str) -> bool {
    unbound_lines(src).next().is_some()
}

/// The unbound spawn lines in one file's shipping code, as `(line, 1-based no)`.
///
/// Tests spawn freely — only shipping code is bound — so everything from the
/// first `#[cfg(test)]` onward is ignored.
fn unbound_lines(src: &str) -> impl Iterator<Item = (&str, usize)> {
    let prod = src.split("#[cfg(test)]").next().unwrap_or("");
    let lines: Vec<&str> = prod.lines().collect();
    let owned: Vec<(&str, usize)> = lines
        .iter()
        .enumerate()
        .filter(|(i, line)| {
            line.contains("tokio::spawn(")
                && !line.trim_start().starts_with("//")
                && !line.contains("spawn-detached:")
                && !lines[..*i].iter().rev().take(4).any(|p| p.contains("spawn-detached:"))
        })
        .map(|(i, line)| (*line, i + 1))
        .collect();
    owned.into_iter()
}

/// Account access from a thread that does not carry the account.
///
/// A tokio task-local rides the task, not the thread. `spawn_blocking` and
/// `std::thread::spawn` run outside it, so `db::` and `STATE` there resolve
/// whoever is live rather than the caller's account — the one way left to write
/// one account's data into another's storage. Nothing in the tree does this;
/// this is what keeps it that way.
pub fn account_access_off_task(crate_root: &Path, src_root: &Path) -> Vec<String> {
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
            let rel = path.strip_prefix(crate_root).unwrap_or(&path).to_string_lossy().replace('\\', "/");
            let Ok(src) = std::fs::read_to_string(&path) else { continue };
            let prod = src.split("#[cfg(test)]").next().unwrap_or("");
            let lines: Vec<&str> = prod.lines().collect();
            for (i, line) in lines.iter().enumerate() {
                if !line.contains("spawn_blocking") && !line.contains("std::thread::spawn") {
                    continue;
                }
                if line.trim_start().starts_with("//") {
                    continue;
                }
                let body = lines[i..(i + 14).min(lines.len())].join("\n");
                if body.contains("db::") || body.contains("STATE.") {
                    offenders.push(format!("{rel}:{}", i + 1));
                }
            }
        }
    }
    offenders.sort();
    offenders
}

/// The assertion both crates run. `pending` is a shrink-only worklist of files
/// not yet converted; it is a ratchet, not an exemption, so a file that no
/// longer spawns unbound must be deleted from it.
pub fn assert_all_spawns_bound(crate_root: &Path, pending: &[&str]) {
    let src_root = crate_root.join("src");
    let offenders: Vec<String> = unbound_spawns(crate_root, &src_root)
        .into_iter()
        .filter(|o| !pending.iter().any(|p| o.starts_with(&format!("{p}:"))))
        .collect();
    assert!(
        offenders.is_empty(),
        "these tasks are not bound to an account — use vector_core::db::spawn_bound so their \
         work follows the account they started under, or mark the site \
         `// spawn-detached: <why it owns no account state>`:\n  {}",
        offenders.join("\n  ")
    );

    let converted: Vec<&&str> = pending
        .iter()
        .filter(|file| {
            std::fs::read_to_string(crate_root.join(file))
                .map(|src| !has_unbound_spawn(&src))
                .unwrap_or(false)
        })
        .collect();
    assert!(
        converted.is_empty(),
        "these files are fully converted — delete them from the pending list so they stay \
         converted:\n  {converted:?}"
    );

    let off_task = account_access_off_task(crate_root, &src_root);
    assert!(
        off_task.is_empty(),
        "a blocking thread runs outside the task, so it does not carry the caller's account — \
         these would read or write whoever is live instead. Do the work inline, or capture the \
         session and re-enter it with db::with_session:\n  {}",
        off_task.join("\n  ")
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_bare_spawn_is_caught_and_a_marked_one_is_not() {
        assert!(has_unbound_spawn("fn f() { tokio::spawn(async {}); }"));
        assert!(!has_unbound_spawn(
            "fn f() { tokio::spawn(async {}); } // spawn-detached: pure CPU."
        ));
        assert!(!has_unbound_spawn(
            "// spawn-detached: pure CPU.\ntokio::spawn(async {});"
        ));
        assert!(!has_unbound_spawn("fn f() { db::spawn_bound(async {}); }"));
    }

    #[test]
    fn the_marker_does_not_reach_past_its_own_site() {
        // Four lines of slack, so a marker above a multi-line setup still
        // applies — but not so far that it silently covers the NEXT spawn.
        let far = format!("// spawn-detached: nope.\n{}tokio::spawn(async {{}});", "\n".repeat(5));
        assert!(has_unbound_spawn(&far));
    }

    #[test]
    fn test_code_spawns_freely() {
        assert!(!has_unbound_spawn("#[cfg(test)]\nmod t { fn f() { tokio::spawn(async {}); } }"));
    }
}
