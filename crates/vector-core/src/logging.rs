//! Runtime log-level gate for the `log_*` macros.
//!
//! Levels, low → high: TRACE < DEBUG < INFO < WARN < ERROR < OFF. A message at
//! level L prints iff `L >= threshold`. Default is WARN (warnings + errors),
//! overridable at launch via `VECTOR_LOG=trace|debug|info|warn|error|off` or at
//! runtime via [`set_log_level`]. `log_info!`/`log_debug!`/`log_trace!` remain
//! compiled out entirely in release; the gate only narrows the dev firehose.

use std::sync::atomic::{AtomicU8, Ordering};

pub const LEVEL_TRACE: u8 = 0;
pub const LEVEL_DEBUG: u8 = 1;
pub const LEVEL_INFO: u8 = 2;
pub const LEVEL_WARN: u8 = 3;
pub const LEVEL_ERROR: u8 = 4;
pub const LEVEL_OFF: u8 = 5;

/// Default when `VECTOR_LOG` is unset: warnings + errors only.
const DEFAULT_LEVEL: u8 = LEVEL_WARN;
/// Sentinel = "not yet initialised from the environment".
const UNINIT: u8 = u8::MAX;

static LOG_LEVEL: AtomicU8 = AtomicU8::new(UNINIT);

fn parse_level(s: &str) -> Option<u8> {
    match s.trim().to_ascii_lowercase().as_str() {
        "trace" => Some(LEVEL_TRACE),
        "debug" => Some(LEVEL_DEBUG),
        "info" => Some(LEVEL_INFO),
        "warn" | "warning" => Some(LEVEL_WARN),
        "error" => Some(LEVEL_ERROR),
        "off" | "none" | "silent" => Some(LEVEL_OFF),
        _ => None,
    }
}

/// Active threshold, initialising from `VECTOR_LOG` on first read (benign race:
/// concurrent initialisers all compute the same value).
#[inline]
pub fn log_level() -> u8 {
    match LOG_LEVEL.load(Ordering::Relaxed) {
        UNINIT => {
            let lvl = std::env::var("VECTOR_LOG")
                .ok()
                .and_then(|v| parse_level(&v))
                .unwrap_or(DEFAULT_LEVEL);
            LOG_LEVEL.store(lvl, Ordering::Relaxed);
            lvl
        }
        v => v,
    }
}

/// Whether a message at `level` should print under the active threshold.
#[inline]
pub fn level_enabled(level: u8) -> bool {
    level >= log_level()
}

/// Override the threshold at runtime (e.g. a settings toggle).
pub fn set_log_level(level: u8) {
    LOG_LEVEL.store(level, Ordering::Relaxed);
}

/// Set the threshold by name (`trace|debug|info|warn|error|off`); returns false
/// on an unknown name.
pub fn set_log_level_str(s: &str) -> bool {
    match parse_level(s) {
        Some(l) => {
            set_log_level(l);
            true
        }
        None => false,
    }
}
