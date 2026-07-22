/// Log macros shared across all Vector clients.
///
/// `log_info!`, `log_debug!`, `log_trace!` compile to no-ops in release builds.
/// `log_warn!` always compiles in (with UTC timestamps). In ALL builds each
/// macro is gated at runtime by the active level (see `crate::logging`): default
/// WARN, override with `VECTOR_LOG=trace|debug|info|warn|error|off`. The level
/// check is cheap and the message args aren't formatted when suppressed.

// Release builds strip the info/debug/trace bodies entirely, so a variable
// referenced only inside one of these macros would go unused. `keep_used!`
// re-references the args in a dead `if false` block: DCE removes it (zero
// runtime cost, args never evaluated) while the borrow checker still counts
// the args as used, so call sites stay warning-free in every profile.
#[macro_export]
#[doc(hidden)]
macro_rules! __log_keep_used {
    ($($arg:tt)*) => {{
        #[cfg(not(debug_assertions))]
        if false { let _ = format_args!($($arg)*); }
    }};
}

#[macro_export]
macro_rules! log_info {
    ($($arg:tt)*) => {{
        #[cfg(debug_assertions)]
        if $crate::logging::level_enabled($crate::logging::LEVEL_INFO) {
            eprintln!("[INFO] {}", format_args!($($arg)*));
        }
        $crate::__log_keep_used!($($arg)*);
    }};
}

#[macro_export]
macro_rules! log_debug {
    ($($arg:tt)*) => {{
        #[cfg(debug_assertions)]
        if $crate::logging::level_enabled($crate::logging::LEVEL_DEBUG) {
            eprintln!("[DEBUG] {}", format_args!($($arg)*));
        }
        $crate::__log_keep_used!($($arg)*);
    }};
}

#[macro_export]
macro_rules! log_trace {
    ($($arg:tt)*) => {{
        #[cfg(debug_assertions)]
        if $crate::logging::level_enabled($crate::logging::LEVEL_TRACE) {
            eprintln!("[TRACE] {}", format_args!($($arg)*));
        }
        $crate::__log_keep_used!($($arg)*);
    }};
}

#[macro_export]
macro_rules! log_warn {
    ($($arg:tt)*) => {{
        if $crate::logging::level_enabled($crate::logging::LEVEL_WARN) {
            let _secs = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs();
            eprintln!("[WARN {:02}:{:02}:{:02}Z] {}", (_secs / 3600) % 24, (_secs / 60) % 60, _secs % 60, format_args!($($arg)*));
        }
    }};
}
