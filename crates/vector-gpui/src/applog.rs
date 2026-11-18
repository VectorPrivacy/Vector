//! The persistent log (`vector.log`, Settings > Copy Logs), shared with Tauri: vector-core's
//! persisted lines, every `log` error (GPUI's included), and panics with their backtrace.

use std::io::Write;
use std::path::PathBuf;
use std::sync::OnceLock;

static FILE: OnceLock<PathBuf> = OnceLock::new();

/// Tauri's `append_vector_log`: one line, the file trimmed to its last 900 once past 1000.
pub fn append(line: &str) {
    let Some(path) = FILE.get() else { return };
    if let Ok(existing) = std::fs::read_to_string(path) {
        let lines: Vec<&str> = existing.lines().collect();
        if lines.len() > 1000 {
            let _ = std::fs::write(path, lines[lines.len() - 900..].join("\n"));
        }
    }
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(path) {
        let _ = writeln!(f, "{line}");
    }
}

/// env_logger on stderr as before, with errors also kept in the file.
struct Logger(env_logger::Logger);

impl log::Log for Logger {
    fn enabled(&self, meta: &log::Metadata) -> bool {
        meta.level() <= log::Level::Error || self.0.enabled(meta)
    }

    fn log(&self, record: &log::Record) {
        if record.level() <= log::Level::Error {
            append(&format!(
                "[{} ERROR] {}: {}", // i18n-exempt: log format
                vector_core::logging::timestamp_utc(),
                record.target(),
                record.args()
            ));
        }
        if self.0.enabled(record.metadata()) {
            self.0.log(record);
        }
    }

    fn flush(&self) {
        self.0.flush();
    }
}

pub fn init(data_dir: Option<PathBuf>) {
    if let Some(dir) = data_dir {
        let _ = FILE.set(dir.join("vector.log"));
    }
    let env = env_logger::Builder::from_default_env().build();
    let max = env.filter().max(log::LevelFilter::Error);
    if log::set_boxed_logger(Box::new(Logger(env))).is_ok() {
        log::set_max_level(max);
    }
    vector_core::logging::set_persist_sink(append);
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        // A panic escaping the hook aborts on the spot, so the report can never be what kills us.
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let backtrace = std::backtrace::Backtrace::force_capture();
            append(&format!(
                "[{} PANIC] {info}\n\nBacktrace:\n{backtrace}\n", // i18n-exempt: log format
                vector_core::logging::timestamp_utc()
            ));
        }));
        previous(info);
    }));
}
