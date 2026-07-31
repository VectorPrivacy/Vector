/// Tauri-specific log macros.
///
/// `log_info!`, `log_debug!`, `log_trace!`, `log_warn!` are defined in vector-core
/// and imported via `#[macro_use] extern crate vector_core` in lib.rs.
///
/// `log_error!` stays here because it writes to the log file and emits a toast
/// to the frontend via TAURI_APP — both Tauri-specific.

macro_rules! log_error {
    ($($arg:tt)*) => {{
        let msg = format!($($arg)*);
        let line = format!("[{} ERROR] {}", vector_core::logging::timestamp_utc(), &msg);
        eprintln!("{}", &line);
        $crate::append_vector_log(&line);
        // Notify user that an error occurred (details are in Settings > Copy Logs)
        if let Some(handle) = $crate::TAURI_APP.get() {
            use tauri::Emitter;
            let _ = handle.emit("show_toast", "Something went wrong — copy logs in Settings for details");
        }
    }};
}
