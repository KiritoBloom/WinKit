//! Event log observability models.

use serde::{Deserialize, Serialize};

/// Windows event severity levels (wevtapi `Level`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EventLevel {
    Critical = 1,
    Error = 2,
    Warning = 3,
    Information = 4,
    Verbose = 5,
    Unknown = 0,
}

impl EventLevel {
    pub fn from_u32(v: u32) -> Self {
        match v {
            1 => Self::Critical,
            2 => Self::Error,
            3 => Self::Warning,
            4 => Self::Information,
            5 => Self::Verbose,
            _ => Self::Unknown,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Critical => "critical",
            Self::Error => "error",
            Self::Warning => "warning",
            Self::Information => "information",
            Self::Verbose => "verbose",
            Self::Unknown => "unknown",
        }
    }
}

/// A normalized event log entry.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct EventInfo {
    pub record_id: Option<u64>,
    pub event_id: Option<u32>,
    pub level: EventLevel,
    /// Provider name (event source), when available.
    pub provider: Option<String>,
    /// Channel/log name, e.g. `Application`, `System`.
    pub channel: Option<String>,
    /// RFC3339 time the event was created.
    pub time_created: Option<String>,
    pub computer: Option<String>,
    pub process_id: Option<u32>,
    /// Rendered message text when the publisher exposes it; may be absent.
    pub message: Option<String>,
}

/// Parameters for a bounded event query.
#[derive(Debug, Clone)]
pub struct EventQuery {
    /// Channel path, e.g. `Application`, `System`, or a provider channel.
    pub log: String,
    /// Minimum severity: 1 (Critical) .. 5 (Verbose).
    pub min_level: Option<u32>,
    /// Only events newer than this many minutes.
    pub since_minutes: Option<u64>,
    /// Restrict to a specific provider name.
    pub provider: Option<String>,
    /// Restrict to a specific event ID.
    pub event_id: Option<u32>,
    pub max_results: usize,
}
