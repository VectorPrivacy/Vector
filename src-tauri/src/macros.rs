/// Log macros that replace the `log` crate.
///
/// `log_info!`, `log_debug!`, `log_trace!` compile to no-ops in release builds.
/// `log_warn!` and `log_error!` always print (important for diagnostics).

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
        eprintln!("[WARN] {}", format_args!($($arg)*));
    }};
}

macro_rules! log_error {
    ($($arg:tt)*) => {{
        eprintln!("[ERROR] {}", format_args!($($arg)*));
    }};
}
