//! Application / browser observability models (generic + Chrome-specific).
//!
//! These models are generic where possible (`ApplicationInfo`, `TabInfo`)
//! so future adapters (Edge, Firefox, VS Code, ...) can reuse them.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// Availability states shared by all application adapters (§55).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ApplicationState {
    /// The application is not installed.
    NotInstalled,
    /// Installed but not running.
    InstalledNotRunning,
    /// Running, but the inspection interface is not available.
    RunningNotInspectable,
    /// Running and the inspection interface is reachable.
    RunningInspectable,
    /// Connected and actively serving data.
    Connected,
}

impl ApplicationState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::NotInstalled => "not_installed",
            Self::InstalledNotRunning => "installed_not_running",
            Self::RunningNotInspectable => "running_not_inspectable",
            Self::RunningInspectable => "running_inspectable",
            Self::Connected => "connected",
        }
    }
}

/// Generic description of a discovered application.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ApplicationInfo {
    pub id: String,
    pub display_name: String,
    pub version: String,
    pub state: ApplicationState,
    /// Capability names this adapter declares (§23).
    pub capabilities: Vec<String>,
    /// Extra adapter-specific details (paths, endpoints, counts).
    pub details: serde_json::Value,
}

/// A Chrome DevTools target.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TargetInfo {
    pub id: String,
    pub title: String,
    pub url: String,
    /// Target type: `page`, `browser`, `service_worker`, `shared_worker`, ...
    pub kind: String,
    pub attachable: bool,
    pub browser_context_id: Option<String>,
}

/// A browser tab (page target), correlated with OS info where available.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TabInfo {
    pub id: String,
    pub title: String,
    pub url: String,
    pub active: bool,
    /// Window ID as reported by the browser, when available.
    pub window_id: Option<String>,
    /// Windows PID when a reliable mapping exists; otherwise `None` and
    /// `process_mapping` explains the uncertainty.
    pub process_id: Option<u32>,
    /// How `process_id` was obtained: `none`, `browser_process_aggregate`, or
    /// `exact`. WinKit never fabricates an exact mapping it cannot produce.
    pub process_mapping: String,
    pub kind: String,
}

/// Browser-wide info from `chrome_info`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct BrowserInfo {
    pub name: String,
    pub version: String,
    pub user_agent: Option<String>,
    pub protocol_version: Option<String>,
    pub web_socket_url: Option<String>,
    pub devtools_port: Option<u16>,
    /// Human-readable availability state.
    pub state: String,
    pub tabs: usize,
    /// Chrome processes reported by `SystemInfo.getProcessInfo`.
    pub processes: Vec<BrowserProcessInfo>,
}

/// A Chrome process from `SystemInfo.getProcessInfo`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct BrowserProcessInfo {
    pub kind: String,
    pub pid: u32,
    pub cpu_time_ms: u64,
    pub name: Option<String>,
}

/// `Performance.getMetrics` result with deltas between two samples.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PerformanceMetrics {
    /// Latest metric values (name -> value).
    pub metrics: BTreeMap<String, f64>,
    /// Change between the two samples (name -> delta).
    pub deltas: BTreeMap<String, f64>,
    pub sample_interval_ms: u64,
    /// Cumulative long-task duration delta in milliseconds.
    pub long_task_ms: f64,
    /// Cumulative main-thread script duration delta in milliseconds.
    pub script_ms: f64,
}

/// Memory picture of one tab (§30).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct MemoryInfo {
    pub js_heap_used_bytes: Option<u64>,
    pub js_heap_total_bytes: Option<u64>,
    pub js_heap_limit_bytes: Option<u64>,
    pub dom_documents: Option<u64>,
    pub dom_nodes: Option<u64>,
    pub js_event_listeners: Option<u64>,
    /// Heap growth observed between two samples (signed).
    pub growth_bytes: Option<i64>,
    pub growth_rate_bytes_per_second: Option<i64>,
}

/// One observed request, sanitized (no headers, no cookies, no bodies).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct NetworkRequestSummary {
    pub method: String,
    /// Sanitized URL: scheme + host + truncated path, no query string.
    pub url: String,
    pub status: Option<u16>,
    pub failed: bool,
    pub error_text: Option<String>,
    pub response_time_ms: Option<u64>,
    /// Encoded data length in bytes, when reported.
    pub bytes: Option<u64>,
    pub mime_type: Option<String>,
}

/// Aggregate network activity for one tab during an observation window.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct NetworkSummary {
    pub observation_ms: u64,
    pub total_requests: usize,
    pub completed: usize,
    pub failed: usize,
    pub failed_ratio: Option<f64>,
    pub bytes_transferred: Option<u64>,
    pub status_buckets: BTreeMap<u16, u32>,
    pub avg_response_ms: Option<f64>,
    pub p95_response_ms: Option<f64>,
    pub top_slowest: Vec<NetworkRequestSummary>,
    pub failures: Vec<NetworkRequestSummary>,
}

/// A console message observed during a runtime window.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ConsoleMessage {
    pub level: String,
    pub text: String,
}

/// Runtime/console picture of one tab (§32).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RuntimeInfo {
    pub observation_ms: u64,
    pub document_url: String,
    pub ready_state: Option<String>,
    pub title: Option<String>,
    pub console_errors: usize,
    pub console_warnings: usize,
    pub exceptions: usize,
    pub console_samples: Vec<ConsoleMessage>,
    pub exception_samples: Vec<String>,
}

/// One sample in a tab time-series (§75).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TrendSample {
    /// Milliseconds since the first sample.
    pub offset_ms: u64,
    /// JS heap usage at this point, when readable.
    pub js_heap_used_bytes: Option<u64>,
    /// Main-thread script duration since the previous sample (ms).
    pub script_ms_delta: f64,
    /// Long-task duration since the previous sample (ms).
    pub long_task_ms_delta: f64,
}

/// Heap growth summary derived from a time series.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TrendMemory {
    pub start_bytes: Option<u64>,
    pub end_bytes: Option<u64>,
    /// End minus start (signed).
    pub delta_bytes: Option<i64>,
    pub growth_rate_bytes_per_second: Option<i64>,
    /// True when the series shows repeated upward movement over multiple
    /// samples, not a single spike.
    pub sustained_growth: bool,
}

/// What changed in one tab over an observation window (§75).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TrendInfo {
    pub tab_id: String,
    pub title: String,
    pub url: String,
    pub duration_ms: u64,
    pub samples: Vec<TrendSample>,
    pub memory: TrendMemory,
    /// Total long-task duration across the window (ms).
    pub long_task_ms: f64,
    /// Total main-thread script duration across the window (ms).
    pub script_ms: f64,
    /// Aggregate CPU of all chrome.exe processes, sampled over the window.
    /// Percent of total system CPU capacity (100% = all cores fully busy).
    pub aggregate_cpu_percent: Option<f64>,
    /// Basis of `aggregate_cpu_percent`: `system_capacity_all_cores`.
    pub cpu_percent_basis: &'static str,
    pub resource_usage: serde_json::Value,
    pub report: crate::models::DiagnosticReport,
}
