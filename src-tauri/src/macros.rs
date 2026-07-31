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
        $crate::append_vector_log(&line);
        // Notify user that an error occurred (details are in Settings > Copy Logs)
        if let Some(handle) = $crate::TAURI_APP.get() {
            use tauri::Emitter;
            let _ = handle.emit("show_toast", "Something went wrong — copy logs in Settings for details");
        }
    }};
}
