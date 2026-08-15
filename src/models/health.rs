//! Machine-wide health models (§76).
//!
//! `system_health` groups running processes by application, aggregates each
//! group's memory and CPU, adds system-level facts (memory pressure, disk
//! space), and lists explicit threshold-based issues. The AI agent reads the
//! structured result instead of querying process tools one by one.

use serde::{Deserialize, Serialize};

/// One application group (all processes of one executable).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ApplicationGroupInfo {
    /// Executable stem, e.g. `chrome`.
    pub name: String,
    /// Human-readable label, e.g. `Google Chrome`.
    pub display_name: String,
    pub process_count: usize,
    pub total_working_set_bytes: u64,
    /// Aggregate CPU percent of total system CPU capacity (100% = all
    /// logical processors fully busy), sampled over
    /// `cpu_percent_sample_ms`.
    pub cpu_percent: Option<f64>,
    /// Basis of `cpu_percent`: `system_capacity_all_cores`. A single busy
    /// core on an N-core machine reports 100/N percent.
    pub cpu_percent_basis: String,
    pub cpu_percent_sample_ms: u64,
    /// `high_cpu`, `high_memory`, `high_cpu_and_memory`, or `normal`.
    pub status: String,
}

/// Disk health for one drive.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct DriveHealth {
    /// Drive label, e.g. `C:`.
    pub drive: String,
    pub total_bytes: Option<u64>,
    pub free_bytes: Option<u64>,
    pub percent_free: Option<f64>,
    pub low_disk_space: bool,
}

/// System-level health facts.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SystemHealth {
    pub memory_load_percent: Option<f64>,
    pub total_memory_bytes: Option<u64>,
    pub available_memory_bytes: Option<u64>,
    /// True when memory load crosses the configured pressure threshold.
    pub memory_pressure: bool,
    pub drives: Vec<DriveHealth>,
}

/// A single explicit issue found by the health check.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct HealthIssue {
    /// `application` or `system`.
    pub layer: String,
    /// App name or drive/system subject.
    pub subject: String,
    /// `high_cpu`, `high_memory`, `low_disk_space`, or `memory_pressure`.
    pub kind: String,
    /// Measured value.
    pub value: String,
    /// Threshold it crossed.
    pub threshold: String,
    /// Deterministic 0-100 severity score (see `docs/diagnostics.md`).
    pub score: u8,
    /// Ranking category: `storage`, `memory_pressure`, `app_cpu`, or
    /// `app_memory`.
    pub category: String,
    /// `low`, `medium`, `high`, or `critical` (score bands).
    pub severity: String,
}

/// The full machine-wide health report.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SystemHealthReport {
    pub generated_at: String,
    /// Sorted by total working set, descending.
    pub applications: Vec<ApplicationGroupInfo>,
    pub system: SystemHealth,
    /// Empty when nothing crossed a threshold. Sorted by `score` descending,
    /// so the first issue is the biggest problem.
    pub issues: Vec<HealthIssue>,
}
