//! Minimal leveled logger writing to stderr.
//!
//! MCP stdio servers must keep stdout clean for protocol messages, so all
//! diagnostics go to stderr. The implementation is intentionally dependency
//! free: a level gate plus timestamped lines.

use std::sync::atomic::{AtomicU8, Ordering};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Level {
    Error = 0,
    Warn = 1,
    Info = 2,
    Debug = 3,
    Trace = 4,
}

impl Level {
    pub fn parse(s: &str) -> Option<Self> {
        match s.to_ascii_lowercase().as_str() {
            "error" => Some(Self::Error),
            "warn" | "warning" => Some(Self::Warn),
            "info" => Some(Self::Info),
            "debug" => Some(Self::Debug),
            "trace" => Some(Self::Trace),
            _ => None,
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Error => "ERROR",
            Self::Warn => "WARN",
            Self::Info => "INFO",
            Self::Debug => "DEBUG",
            Self::Trace => "TRACE",
        }
    }
}

static CURRENT: AtomicU8 = AtomicU8::new(Level::Info as u8);

/// Set the global log level. Should be called once at startup.
pub fn set_level(level: Level) {
    CURRENT.store(level as u8, Ordering::Relaxed);
}

/// Whether messages at `level` will be emitted.
pub fn enabled(level: Level) -> bool {
    level as u8 <= CURRENT.load(Ordering::Relaxed)
}

/// ISO-8601-ish local timestamp prefix, e.g. `2026-08-13T12:00:00.000Z`.
pub fn timestamp() -> String {
    let now = std::time::SystemTime::now();
    match now.duration_since(std::time::UNIX_EPOCH) {
        Ok(d) => {
            let secs = d.as_secs();
            let millis = d.subsec_millis();
            let days = secs / 86_400;
            let (year, month, day) = civil_from_days(days as i64);
            let (hour, minute, second) = ((secs / 3600) % 24, (secs / 60) % 60, secs % 60);
            format!("{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}.{millis:03}Z")
        }
        Err(_) => String::from("1970-01-01T00:00:00.000Z"),
    }
}

/// Convert a number of days since 1970-01-01 into a (year, month, day) tuple.
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

fn emit(level: Level, msg: &str) {
    eprintln!("[{}][{}] {}", timestamp(), level.label(), msg);
}

/// Emit a log line if the level is enabled. Returns the message for
/// convenience in tests.
#[macro_export]
macro_rules! log_error {
    ($($arg:tt)*) => {
        if $crate::utils::log::enabled($crate::utils::log::Level::Error) {
            $crate::utils::log::emit_public($crate::utils::log::Level::Error, &format!($($arg)*));
        }
    };
}

#[macro_export]
macro_rules! log_warn {
    ($($arg:tt)*) => {
        if $crate::utils::log::enabled($crate::utils::log::Level::Warn) {
            $crate::utils::log::emit_public($crate::utils::log::Level::Warn, &format!($($arg)*));
        }
    };
}

#[macro_export]
macro_rules! log_info {
    ($($arg:tt)*) => {
        if $crate::utils::log::enabled($crate::utils::log::Level::Info) {
            $crate::utils::log::emit_public($crate::utils::log::Level::Info, &format!($($arg)*));
        }
    };
}

#[macro_export]
macro_rules! log_debug {
    ($($arg:tt)*) => {
        if $crate::utils::log::enabled($crate::utils::log::Level::Debug) {
            $crate::utils::log::emit_public($crate::utils::log::Level::Debug, &format!($($arg)*));
        }
    };
}

#[macro_export]
macro_rules! log_trace {
    ($($arg:tt)*) => {
        if $crate::utils::log::enabled($crate::utils::log::Level::Trace) {
            $crate::utils::log::emit_public($crate::utils::log::Level::Trace, &format!($($arg)*));
        }
    };
}

/// Public entry point used by the exported macros.
pub fn emit_public(level: Level, msg: &str) {
    emit(level, msg);
}
