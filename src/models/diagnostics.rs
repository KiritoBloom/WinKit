//! Diagnostics models: measurements, signals, correlations, possible causes,
//! and machine-wide diagnosis (§73, §74, §77).
//!
//! The diagnostic engine is deterministic and heuristic. It never claims a
//! root cause it cannot support; it separates raw measurements from signal
//! interpretation and possible-cause hypotheses, and lets the AI agent
//! produce the final explanation. `measurements` are facts, `signals` are
//! threshold-based interpretations of those facts, and `possible_causes` are
//! heuristic hypotheses supported by signal combinations.

use serde::{Deserialize, Serialize};

/// A single measured fact — raw, without interpretation.
///
/// Signals reference measurements by `metric` (and `subject` when the metric
/// is per-application or per-drive), so an agent can trace every signal back
/// to the exact value that produced it.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Measurement {
    /// Metric name, e.g. `cpu_percent`, `js_heap_used_bytes`,
    /// `drive_free_percent`. Matches `EvidencePoint.metric` on signals.
    pub metric: String,
    /// Human-readable measured value, e.g. `57.0% of system CPU capacity`.
    pub value: String,
    /// Raw numeric value for agent-side math, when available.
    pub value_number: Option<f64>,
    /// Unit of `value_number`, e.g. `bytes`, `milliseconds`, `percent`.
    pub unit: String,
    /// `tab`, `browser_aggregate`, `application`, or `system`.
    pub scope: String,
    /// App display name, drive label, or other subject when the metric is
    /// per-subject (e.g. `working_set_bytes` for `Google Chrome`).
    pub subject: Option<String>,
    /// What was measured and how, so the number is not misread.
    pub detail: String,
}

/// A single measured fact, used in signal and finding evidence.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct EvidencePoint {
    /// Metric name, e.g. `cpu_percent`, `js_heap_used_bytes`.
    pub metric: String,
    /// Human-readable measured value.
    pub value: String,
    /// Why this evidence matters.
    pub detail: String,
}

/// An unusual observation derived from measurements by threshold rules.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct DiagnosticSignal {
    /// Snake-case kind, e.g. `high_cpu`.
    pub kind: String,
    pub label: String,
    /// `low`, `medium`, or `high`.
    pub severity: String,
    /// The measurements backing this signal (`metric` references the report's
    /// `measurements` list).
    pub evidence: Vec<EvidencePoint>,
}

/// A documented relationship between signals.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct DiagnosticCorrelation {
    pub description: String,
    /// Kinds of the signals involved.
    pub signals: Vec<String>,
    /// 0.0 .. 1.0.
    pub confidence: f64,
}

/// A heuristic hypothesis supported by signals.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PossibleCause {
    pub hypothesis: String,
    pub supporting_signals: Vec<String>,
    /// `low`, `medium`, or `high`.
    pub confidence: String,
    pub confidence_value: f64,
    /// The reasoning chain in plain language.
    pub reasoning: String,
}

/// A complete deterministic diagnostic report (§33, §73).
///
/// Evidence-first shape: `measurements` (facts) are always separated from
/// `signals` (interpretations) and `possible_causes` (hypotheses), so an
/// interpreting agent cannot conflate "CPU was 57%" with "CPU is the
/// problem".
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct DiagnosticReport {
    /// What was analyzed, e.g. `chrome tab <id>` or `windows machine`.
    pub target: String,
    /// RFC3339 generation time.
    pub generated_at: String,
    /// `signals_detected` or `no_supported_signal_detected`.
    ///
    /// A negative result is an explicit statement, not an absence: the
    /// measured evidence stayed below every documented heuristic threshold.
    pub status: String,
    /// `full` or `limited`. `limited` means a core measurement (JS heap or
    /// process CPU for tabs; memory or application evidence for the machine)
    /// was unavailable, so the report is weaker than usual.
    pub evidence_completeness: String,
    /// Every raw measured fact, separate from any interpretation of it.
    pub measurements: Vec<Measurement>,
    pub signals: Vec<DiagnosticSignal>,
    pub correlations: Vec<DiagnosticCorrelation>,
    pub possible_causes: Vec<PossibleCause>,
    /// Honest statements about what this report cannot determine.
    pub limitations: Vec<String>,
    /// A direct instruction to the interpreting AI agent so it does not
    /// over-read the report.
    pub agent_guidance: String,
}

/// Raw inputs to the diagnostic engine, collected by adapters.
#[derive(Debug, Clone, Default)]
pub struct TabDiagnosticData {
    pub cpu_percent: Option<f64>,
    pub js_heap_used_bytes: Option<u64>,
    pub heap_growth_bytes_per_second: Option<i64>,
    pub long_task_ms: f64,
    pub script_ms: f64,
    pub dom_nodes: Option<u64>,
    pub total_requests: usize,
    pub failed_requests: usize,
    pub avg_response_ms: Option<f64>,
    pub p95_response_ms: Option<f64>,
    pub bytes_transferred: Option<u64>,
    pub console_errors: usize,
    pub exceptions: usize,
    /// True when heap samples show repeated upward movement (from the
    /// time-series trend tool). Single snapshots leave this `false`.
    pub heap_growth_sustained: bool,
}

// --- Machine-wide diagnosis (§77) -----------------------------------------

/// One drive's evidence for a machine-wide diagnosis.
#[derive(Debug, Clone, Default)]
pub struct SystemDriveEvidence {
    /// Drive label, e.g. `C:`.
    pub subject: String,
    pub free_bytes: u64,
    pub total_bytes: u64,
}

/// One application group's evidence for a machine-wide diagnosis.
#[derive(Debug, Clone, Default)]
pub struct SystemAppEvidence {
    /// Executable stem, e.g. `chrome`.
    pub name: String,
    /// Human-readable label, e.g. `Google Chrome`.
    pub display_name: String,
    /// Number of running processes in this group.
    pub process_count: usize,
    pub working_set_bytes: u64,
    /// Percent of total system CPU capacity (100% = all cores busy).
    pub cpu_percent: Option<f64>,
}

/// Thermal evidence for a machine-wide diagnosis (from `thermal_snapshot`).
#[derive(Debug, Clone, Default)]
pub struct SystemThermalEvidence {
    /// `low`, `elevated`, `high`, or `unknown`.
    pub cpu_thermal_pressure: String,
    /// `likely`, `not_observed`, or `unknown`.
    pub cpu_throttling: String,
    /// True when the CPU is running well below its base clock, when known.
    pub cpu_frequency_reduced: Option<bool>,
    /// Highest readable CPU temperature (C), when any CPU sensor exists.
    pub cpu_temperature_c: Option<f64>,
    /// `low`, `elevated`, `high`, or `unknown`.
    pub gpu_thermal_pressure: String,
}

/// One physical disk's health evidence for a machine-wide diagnosis.
#[derive(Debug, Clone, Default)]
pub struct SystemStorageHealthEvidence {
    /// Device name, e.g. `PhysicalDrive0`.
    pub device: String,
    /// `nvme`, `sata`, `usb`, or `unknown`.
    pub interface: String,
    /// `healthy`, `warning`, `critical`, or `unknown`.
    pub health_status: Option<String>,
    pub temperature_c: Option<f64>,
    /// NVMe percentage used (0-100).
    pub percentage_used: Option<u8>,
}

/// Battery evidence for a machine-wide diagnosis.
#[derive(Debug, Clone, Default)]
pub struct SystemBatteryEvidence {
    pub present: bool,
    pub percent: Option<u8>,
    pub ac_online: Option<bool>,
    pub charging: Option<bool>,
    /// `charging`, `discharging`, `critical`, `low`, or `unknown`.
    pub battery_state: Option<String>,
    /// full_charge / design capacity as a percentage.
    pub health_percent: Option<f64>,
}

/// One Wi-Fi adapter's evidence for a machine-wide diagnosis.
#[derive(Debug, Clone, Default)]
pub struct SystemWifiEvidence {
    pub description: String,
    /// `connected`, `disconnected`, or `not_available`.
    pub state: String,
    pub signal_percent: Option<u8>,
    pub link_speed_mbps: Option<f64>,
}

/// Evidence inputs for a machine-wide diagnosis.
#[derive(Debug, Clone, Default)]
pub struct SystemDiagnosticData {
    pub memory_load_percent: Option<f64>,
    pub memory_available_bytes: Option<u64>,
    pub memory_total_bytes: Option<u64>,
    /// System available-memory change in bytes/second (positive = memory
    /// being consumed), sampled across a short window.
    pub memory_growth_bytes_per_second: Option<i64>,
    pub drives: Vec<SystemDriveEvidence>,
    pub app_groups: Vec<SystemAppEvidence>,
    pub thermal: Option<SystemThermalEvidence>,
    pub storage_health: Vec<SystemStorageHealthEvidence>,
    pub battery: Option<SystemBatteryEvidence>,
    pub wifi: Vec<SystemWifiEvidence>,
}

/// One deterministically ranked finding (§77).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RankedFinding {
    /// 1-based position after sorting by `score` descending.
    pub rank: usize,
    /// e.g. `Critical storage pressure`.
    pub title: String,
    /// `storage`, `memory_pressure`, `app_cpu`, `app_memory`, or
    /// `memory_growth`.
    pub category: String,
    /// `critical`, `high`, `medium`, or `low` (score bands, see
    /// `docs/diagnostics.md`).
    pub severity: String,
    /// `high`, `medium`, or `low`, derived from the same deterministic score.
    pub confidence: String,
    /// Deterministic 0-100 severity score; higher = worse. The formula is
    /// documented and never arbitrary.
    pub score: u8,
    /// The subject the finding is about, e.g. `C:` or `Google Chrome`.
    pub subject: String,
    /// The measurements backing this finding.
    pub evidence: Vec<EvidencePoint>,
    /// Plain-language explanation of why this was flagged.
    pub detail: String,
}

/// The machine-wide diagnosis result (§77).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SystemDiagnosis {
    /// Same evidence-first shape as tab reports.
    pub report: DiagnosticReport,
    /// Findings ranked by deterministic score, descending.
    pub findings: Vec<RankedFinding>,
    /// Dimensions that were checked and found clean ("no evidence of ...").
    /// Only dimensions actually measured appear here.
    pub checked_clean: Vec<String>,
}
