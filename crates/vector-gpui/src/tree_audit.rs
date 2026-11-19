//! The core's tree checks, run over this shell: tasks bound to their account, and times read
//! from Vector's clock rather than the device's.

use std::path::Path;

fn root() -> &'static Path {
    Path::new(env!("CARGO_MANIFEST_DIR"))
}

#[test]
fn per_account_tasks_are_spawned_bound_to_their_account() {
    vector_core::spawn_audit::assert_all_spawns_bound(root(), &[]);
}

#[test]
fn no_blocking_thread_reaches_an_account_off_its_task() {
    let r = root();
    let found = vector_core::spawn_audit::account_access_off_task(r, &r.join("src"));
    assert!(found.is_empty(), "use vector_core::db::blocking_bound:\n  {}", found.join("\n  "));
}

/// chrono's `now` as well as the core's patterns: the shell reads local dates through it.
#[test]
fn nothing_in_the_shell_reads_the_device_clock_unmarked() {
    let r = root();
    let mut found = vector_core::clock::device_clock_reads(r, &r.join("src"));
    found.extend(chrono_reads(&r.join("src")).into_iter().map(|(file, n)| format!("{}:{n}", file.strip_prefix(r).unwrap_or(&file).display())));
    assert!(
        found.is_empty(),
        "these read the device's clock: use vector_core::clock, or mark the site `// device-clock: <why>`:\n  {}",
        found.join("\n  ")
    );
}

fn chrono_reads(dir: &Path) -> Vec<(std::path::PathBuf, usize)> {
    let mut found = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        for entry in std::fs::read_dir(&d).into_iter().flatten().flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
                continue;
            }
            if path.extension().and_then(|e| e.to_str()) != Some("rs") || path.ends_with("tree_audit.rs") {
                continue;
            }
            let Ok(src) = std::fs::read_to_string(&path) else { continue };
            let lines: Vec<&str> = src.lines().collect();
            let cut = lines.windows(2).position(|w| w[0].trim() == "#[cfg(test)]" && w[1].trim_start().starts_with("mod ")).unwrap_or(lines.len());
            for i in 0..cut {
                let l = lines[i];
                let reads = ["Utc::now(", "Local::now(", "Utc::now)", "Local::now)"].iter().any(|p| l.contains(p));
                if reads && !l.trim_start().starts_with("//") && !lines[i.saturating_sub(3)..=i].iter().any(|p| p.contains("device-clock:")) {
                    found.push((path.clone(), i + 1));
                }
            }
        }
    }
    found
}
