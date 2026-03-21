/// Log macros that replace the `log` crate.
///
/// `log_info!`, `log_debug!`, `log_trace!` compile to no-ops in release builds.
/// `log_warn!` and `log_error!` always print with UTC timestamps (important for diagnostics).
/// `log_error!` also emits a toast to the frontend so users know something went wrong.

#[allow(unused_macros)]


macro_rules! log_info {
    ($($arg:tt)*) => {{
        #[cfg(debug_assertions)]
        eprintln!("[INFO] {}", format_args!($($arg)*));
    }};
}

macro_rules! log_debug {
    ($($arg:tt)*) => {{
        #[cfg(debug_assertions)]
        eprintln!("[DEBUG] {}", format_args!($($arg)*));
    }};
}

macro_rules! log_trace {
    ($($arg:tt)*) => {{
        #[cfg(debug_assertions)]
        eprintln!("[TRACE] {}", format_args!($($arg)*));
    }};
}

macro_rules! log_warn {
    ($($arg:tt)*) => {{
        let _secs = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        eprintln!("[WARN {:02}:{:02}:{:02}Z] {}", (_secs / 3600) % 24, (_secs / 60) % 60, _secs % 60, format_args!($($arg)*));
    }};
}

macro_rules! log_error {
    ($($arg:tt)*) => {{
        let _secs = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        let msg = format!($($arg)*);
        let line = format!("[ERROR {:02}:{:02}:{:02}Z] {}", (_secs / 3600) % 24, (_secs / 60) % 60, _secs % 60, &msg);
        eprintln!("{}", &line);
        // Append to vector.log (capped at 1000 lines)
        if let Ok(data_dir) = $crate::account_manager::get_app_data_dir() {
            let log_path = data_dir.join("vector.log");
            // Trim to 1000 lines if over limit
            if let Ok(existing) = std::fs::read_to_string(&log_path) {
                let lines: Vec<&str> = existing.lines().collect();
                if lines.len() > 1000 {
                    let trimmed = lines[lines.len() - 900..].join("\n");
                    let _ = std::fs::write(&log_path, trimmed);
                }
            }
            use std::io::Write;
            if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(&log_path) {
                let _ = writeln!(f, "{}", &line);
            }
        }
        // Notify user that an error occurred (details are in Settings > Copy Logs)
        if let Some(handle) = $crate::TAURI_APP.get() {
            use tauri::Emitter;
            let _ = handle.emit("show_toast", "Something went wrong — copy logs in Settings for details");
        }
    }};
}
