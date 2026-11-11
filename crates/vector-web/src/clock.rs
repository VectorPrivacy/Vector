//! The clock nostr reads through `universal-time`. Without a registered
//! provider its time source links as a null function on wasm.

use std::sync::OnceLock;
use std::time::Duration;

use universal_time::{Instant, MonotonicClock, SystemTime, WallClock};

struct BrowserClock;

impl WallClock for BrowserClock {
    fn system_time(&self) -> SystemTime {
        SystemTime::from_unix_duration(Duration::from_secs_f64(js_sys::Date::now() / 1000.0))
    }
}

impl MonotonicClock for BrowserClock {
    fn instant(&self) -> Instant {
        static ORIGIN: OnceLock<web_time::Instant> = OnceLock::new();
        Instant::from_ticks(ORIGIN.get_or_init(web_time::Instant::now).elapsed())
    }
}

universal_time::define_time_provider!(BrowserClock);
