/// Log macros shared across all Vector clients.
///
/// `log_info!`, `log_debug!`, `log_trace!` compile to no-ops in release builds.
/// `log_warn!` always prints with UTC timestamps (important for diagnostics).

#[macro_export]
macro_rules! log_info {
    ($($arg:tt)*) => {{
        #[cfg(debug_assertions)]
        eprintln!("[INFO] {}", format_args!($($arg)*));
    }};
}

#[macro_export]
macro_rules! log_debug {
    ($($arg:tt)*) => {{
        #[cfg(debug_assertions)]
        eprintln!("[DEBUG] {}", format_args!($($arg)*));
    }};
}

#[macro_export]
macro_rules! log_trace {
    ($($arg:tt)*) => {{
        #[cfg(debug_assertions)]
        eprintln!("[TRACE] {}", format_args!($($arg)*));
    }};
}

#[macro_export]
macro_rules! log_warn {
    ($($arg:tt)*) => {{
        let _secs = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        eprintln!("[WARN {:02}:{:02}:{:02}Z] {}", (_secs / 3600) % 24, (_secs / 60) % 60, _secs % 60, format_args!($($arg)*));
    }};
}
