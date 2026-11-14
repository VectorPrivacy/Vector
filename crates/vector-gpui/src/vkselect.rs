//! The Vulkan loader loads every installed driver, and some never unload. Before anything else
//! starts, a forked probe names the drivers behind this machine's real GPUs and the loader is
//! told to load only those. Any driver it can't name leaves the loader as it was. The answer is
//! cached against the GPUs and drivers installed, so the probe runs once per change.

pub fn narrow() {
    const CHOSEN: [&str; 3] = ["VK_LOADER_DRIVERS_SELECT", "VK_DRIVER_FILES", "VK_ICD_FILENAMES"];
    if CHOSEN.iter().any(|v| std::env::var_os(v).is_some()) || std::env::var_os("WGPU_BACKEND").is_some() {
        return;
    }
    let print = fingerprint();
    let cache = dirs::cache_dir().map(|d| d.join("io.vectorapp.gpui").join("vulkan-drivers"));
    let cached = cache
        .as_ref()
        .and_then(|c| std::fs::read_to_string(c).ok())
        .and_then(|s| s.strip_prefix(&format!("{print}\n")).map(str::to_string));
    let select = match cached {
        Some(select) => select,
        None => {
            let select = choose().unwrap_or_default();
            if let Some(c) = &cache {
                let _ = c.parent().map(std::fs::create_dir_all);
                let _ = std::fs::write(c, format!("{print}\n{select}"));
            }
            select
        }
    };
    if !select.is_empty() {
        // SAFETY: called first thing in main, before any other thread exists.
        unsafe { std::env::set_var("VK_LOADER_DRIVERS_SELECT", &select) };
    }
}

/// The drivers to keep, checked by a second probe under the narrowed loader.
fn choose() -> Option<String> {
    let (n, select) = forked(|| probe(None).map(|(g, n)| format!("{n} {g}")))?
        .split_once(' ')
        .map(|(n, g)| (n.parse::<usize>().ok(), g.to_string()))?;
    let n = n?;
    // SAFETY: as in `narrow`; the variable is set only for the checking child's sake.
    unsafe { std::env::set_var("VK_LOADER_DRIVERS_SELECT", &select) };
    let kept = forked(|| probe(Some(n)).map(|_| String::new())).is_some();
    // SAFETY: as above.
    unsafe { std::env::remove_var("VK_LOADER_DRIVERS_SELECT") };
    kept.then_some(select)
}

/// What the choice depends on: the GPUs present and the driver manifests installed.
fn fingerprint() -> String {
    let mut parts = Vec::new();
    if let Ok(cards) = std::fs::read_dir("/sys/class/drm") {
        for card in cards.flatten() {
            let dev = card.path().join("device");
            for f in ["vendor", "device"] {
                if let Ok(v) = std::fs::read_to_string(dev.join(f)) {
                    parts.push(v.trim().to_string());
                }
            }
        }
    }
    let mut dirs = vec!["/usr/share/vulkan/icd.d".into(), "/usr/local/share/vulkan/icd.d".into(), "/etc/vulkan/icd.d".into()];
    if let Some(d) = dirs::data_dir() {
        dirs.push(d.join("vulkan/icd.d"));
    }
    for d in dirs {
        let d: std::path::PathBuf = d;
        if let Ok(entries) = std::fs::read_dir(&d) {
            for e in entries.flatten() {
                let modified = e.metadata().and_then(|m| m.modified()).ok();
                let secs = modified.and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok()).map_or(0, |d| d.as_secs());
                parts.push(format!("{}@{secs}", e.path().display()));
            }
        }
    }
    parts.sort();
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    parts.hash(&mut h);
    format!("{:016x}", h.finish())
}

/// Runs `f` in a forked child, so whatever it loads goes with it; its answer comes back
/// over a pipe.
fn forked(f: impl FnOnce() -> Option<String>) -> Option<String> {
    use std::io::Read;
    use std::os::fd::FromRawFd;
    let mut fds = [0; 2];
    // SAFETY: plain POSIX calls; the process is single-threaded, so the child may allocate.
    unsafe {
        if libc::pipe(fds.as_mut_ptr()) != 0 {
            return None;
        }
        match libc::fork() {
            -1 => {
                libc::close(fds[0]);
                libc::close(fds[1]);
                None
            }
            0 => {
                libc::close(fds[0]);
                if let Some(s) = f() {
                    libc::write(fds[1], s.as_ptr().cast(), s.len());
                    libc::_exit(0);
                }
                libc::_exit(1);
            }
            child => {
                libc::close(fds[1]);
                let mut out = String::new();
                let _ = std::fs::File::from_raw_fd(fds[0]).read_to_string(&mut out);
                let mut status = 0;
                libc::waitpid(child, &mut status, 0);
                (libc::WIFEXITED(status) && libc::WEXITSTATUS(status) == 0).then_some(out)
            }
        }
    }
}

/// The glob list for the adapters worth keeping, and how many there are; with `expect`, only
/// whether that many are still found.
fn probe(expect: Option<usize>) -> Option<(String, usize)> {
    let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
        backends: wgpu::Backends::VULKAN,
        flags: wgpu::InstanceFlags::empty(),
        backend_options: wgpu::BackendOptions::default(),
        memory_budget_thresholds: wgpu::MemoryBudgetThresholds::default(),
        display: None,
    });
    let infos: Vec<_> = gpui::block_on(instance.enumerate_adapters(wgpu::Backends::VULKAN))
        .iter()
        .map(|a| a.get_info())
        .collect();
    let hardware: Vec<_> = infos.iter().filter(|i| i.device_type != wgpu::DeviceType::Cpu).collect();
    let keep = if hardware.is_empty() { infos.iter().collect() } else { hardware };
    if keep.is_empty() {
        return None;
    }
    if let Some(n) = expect {
        return (keep.len() >= n).then(|| (String::new(), n));
    }
    let mut globs: Vec<&str> = Vec::new();
    for info in &keep {
        let glob = manifest(&info.driver)?;
        if !globs.contains(&glob) {
            globs.push(glob);
        }
    }
    Some((globs.join(","), keep.len()))
}

/// The manifest-name glob of a driver, by the name it reports.
fn manifest(driver: &str) -> Option<&'static str> {
    Some(match driver {
        "radv" => "*radeon*",
        "llvmpipe" => "*lvp*",
        "NVIDIA" => "*nvidia*",
        "NVK" => "*nouveau*",
        d if d.starts_with("Intel open-source Mesa") => "*intel*",
        d if d.starts_with("AMD open-source") || d.starts_with("AMD proprietary") => "*amd*",
        _ => return None,
    })
}
