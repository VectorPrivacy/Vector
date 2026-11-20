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
    #[cfg(target_os = "android")]
    logcat::capture();
    #[cfg(all(target_os = "android", any(debug_assertions, feature = "test-build")))]
    {
        use vector_media::android::storage;
        if let Some(dir) = storage::public_dir().or_else(storage::external_media_dir) {
            logcat::mirror::into(std::path::Path::new(&dir));
        }
    }
    let mut env = env_logger::Builder::from_default_env();
    // No RUST_LOG on a phone: logcat is the only place a launch that never draws can be read.
    #[cfg(target_os = "android")]
    env.parse_filters(if cfg!(any(debug_assertions, feature = "test-build")) { logcat::TEST_FILTER } else { "warn" });
    let env = env.build();
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

/// Android points stdout and stderr at /dev/null: both go to logcat instead, so core's lines,
/// `log` records and the panic report can be read there.
#[cfg(target_os = "android")]
mod logcat {
    use std::ffi::{c_char, c_int};
    use std::io::{BufRead, BufReader};
    use std::os::fd::FromRawFd;

    pub const TEST_FILTER: &str = "info,wgpu_core=warn,wgpu_hal=warn,naga=warn,arti_client=warn,tor_=warn";

    #[link(name = "log")]
    unsafe extern "C" {
        fn __android_log_write(prio: c_int, tag: *const c_char, text: *const c_char) -> c_int;
    }

    fn priority(line: &[u8]) -> c_int {
        let head = &line[..line.len().min(48)];
        let has = |word: &[u8]| head.windows(word.len()).any(|w| w == word);
        if has(b"ERROR") || has(b"PANIC") || has(b"panicked") {
            6
        } else if has(b"WARN") {
            5
        } else {
            4
        }
    }

    pub fn capture() {
        let mut fds: [c_int; 2] = [0; 2];
        // SAFETY: a fresh pipe, whose read end the drain owns.
        let read = unsafe {
            if libc::pipe(fds.as_mut_ptr()) != 0 {
                return;
            }
            std::fs::File::from_raw_fd(fds[0])
        };
        let drain = std::thread::Builder::new().name("logcat".into()).spawn(move || {
            let mut read = BufReader::new(read);
            let mut line = Vec::with_capacity(256);
            while matches!(read.read_until(b'\n', &mut line), Ok(n) if n > 0) {
                if line.last() == Some(&b'\n') {
                    line.pop();
                }
                for b in line.iter_mut().filter(|b| **b == 0) {
                    *b = b' ';
                }
                #[cfg(any(debug_assertions, feature = "test-build"))]
                mirror::line(&line);
                line.push(0);
                // SAFETY: both strings are NUL-terminated and outlive the call.
                unsafe { __android_log_write(priority(&line), c"vector".as_ptr(), line.as_ptr().cast()) };
                line.clear();
            }
        });
        // SAFETY: fds 1 and 2 take the write end only once something drains it; a pipe with no
        // reader would block every write once full.
        unsafe {
            if drain.is_ok() {
                libc::dup2(fds[1], 1);
                libc::dup2(fds[1], 2);
            }
            libc::close(fds[1]);
        }
    }

    /// Test builds also keep a launch's output where the phone's Files app can read it, so a
    /// test build that never draws can be diagnosed without adb.
    #[cfg(any(debug_assertions, feature = "test-build"))]
    pub mod mirror {
        use std::io::Write;
        use std::path::Path;
        use std::sync::Mutex;

        enum Mirror {
            Waiting(Vec<u8>),
            Open(std::fs::File),
            Off,
        }

        static MIRROR: Mutex<Mirror> = Mutex::new(Mirror::Waiting(Vec::new()));
        const HELD: usize = 512 * 1024;

        pub(super) fn line(text: &[u8]) {
            let mut mirror = MIRROR.lock().unwrap_or_else(|e| e.into_inner());
            match &mut *mirror {
                Mirror::Waiting(held) if held.len() < HELD => {
                    held.extend_from_slice(text);
                    held.push(b'\n');
                }
                Mirror::Open(file) => {
                    let _ = file.write_all(text).and_then(|()| file.write_all(b"\n"));
                }
                _ => {}
            }
        }

        /// From here on, and everything since launch, into `dir`/vector-gpui-debug.log.
        pub fn into(dir: &Path) {
            let mut mirror = MIRROR.lock().unwrap_or_else(|e| e.into_inner());
            let Mirror::Waiting(held) = &*mirror else { return };
            let opened = std::fs::create_dir_all(dir)
                .and_then(|()| std::fs::File::create(dir.join("vector-gpui-debug.log")))
                .and_then(|mut file| file.write_all(held).map(|()| file));
            *mirror = match opened {
                Ok(file) => Mirror::Open(file),
                Err(_) => Mirror::Off,
            };
        }
    }
}
