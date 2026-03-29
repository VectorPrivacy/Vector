/// Tauri-specific log macros.
///
/// `log_info!`, `log_debug!`, `log_trace!`, `log_warn!` are defined in vector-core
/// and imported via `#[macro_use] extern crate vector_core` in lib.rs.
///
/// `log_error!` stays here because it writes to the log file and emits a toast
/// to the frontend via TAURI_APP — both Tauri-specific.

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
