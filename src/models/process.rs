//! Process observability models.

use serde::{Deserialize, Serialize};

/// A point-in-time view of one process.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ProcessInfo {
    pub pid: u32,
    pub name: String,
    /// Parent process ID; `None` when it could not be determined.
    pub parent_pid: Option<u32>,
    /// Full executable path, when available.
    pub executable_path: Option<String>,
    /// Command line, when available (best-effort, x64 processes only).
    pub command_line: Option<String>,
    pub working_set_bytes: Option<u64>,
    pub private_bytes: Option<u64>,
    pub threads: Option<u32>,
    /// RFC3339 start time, when available.
    pub start_time: Option<String>,
    /// Total CPU time (kernel + user) in milliseconds.
    pub cpu_time_ms: Option<u64>,
    /// Per-process CPU percent over a short two-sample window (≈300 ms), on
    /// basis `system_capacity_all_cores`. Computed by `get_process` only,
    /// and only when the process handle is openable and both samples
    /// succeed; otherwise `None`. `list_processes` and
    /// `list_processes_minimal` report `None` here by design, because a
    /// per-process percent requires an extra two-sample pass per PID. For
    /// aggregate CPU evidence use `ApplicationGroupInfo.cpu_percent`
    /// (`system_health` / `system_diagnose`), which carries an explicit
    /// `cpu_percent_basis`.
    pub cpu_percent: Option<f64>,
}

/// Memory counters for a single process.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ProcessMemory {
    pub working_set_bytes: u64,
    pub private_bytes: u64,
    pub peak_working_set_bytes: u64,
}

/// Raw CPU times used for sampling.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CpuTime {
    /// Kernel + user time across all CPUs.
    pub system_ms: u64,
    /// Kernel + user time for one process.
    pub process_ms: u64,
}

/// The process that owns a listening port.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ProcessOnPort {
    pub port: u16,
    pub protocol: String,
    pub pid: Option<u32>,
    pub process_name: Option<String>,
    pub state: Option<String>,
}

/// A node in a process tree.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ProcessTreeNode {
    pub pid: u32,
    pub name: String,
    pub parent_pid: Option<u32>,
    pub working_set_bytes: Option<u64>,
    pub threads: Option<u32>,
    pub cpu_time_ms: Option<u64>,
    pub depth: u32,
    pub children: Vec<ProcessTreeNode>,
}
