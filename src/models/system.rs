//! System-level models: OS info, resource snapshots, developer environment.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// Operating-system-level information (safe subset).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SystemInfo {
    pub os_name: String,
    pub version: String,
    pub build: u32,
    pub architecture: String,
    /// Uptime in seconds since boot.
    pub uptime_seconds: u64,
    /// RFC3339 boot time, when derivable.
    pub boot_time: Option<String>,
    pub hostname: Option<String>,
    /// Physical processor core count (matches `hardware_snapshot.cores`).
    pub cpu_cores: u32,
    /// Logical processors (threads), from `GetSystemInfo` — this is what
    /// was previously mislabeled as `cpu_cores`.
    pub logical_processors: u32,
    pub total_memory_bytes: Option<u64>,
}

/// A CPU sample from `GetSystemTimes`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CpuSnapshot {
    pub idle_ms: u64,
    pub kernel_ms: u64,
    pub user_ms: u64,
}

impl CpuSnapshot {
    /// Busy percentage across two samples: the share of total CPU time
    /// across all logical processors that was not idle. 100.0 means every
    /// core was fully busy; a single busy core on an N-core machine reports
    /// 100/N percent.
    pub fn busy_percent(&self, prev: &CpuSnapshot) -> Option<f64> {
        let total = (self.kernel_ms.saturating_sub(prev.kernel_ms))
            .saturating_add(self.user_ms.saturating_sub(prev.user_ms));
        if total == 0 {
            return None;
        }
        let idle = self.idle_ms.saturating_sub(prev.idle_ms);
        let busy = total.saturating_sub(idle);
        Some((busy as f64 / total as f64) * 100.0)
    }
}

/// Aggregate resource view used by `snapshot`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ResourceSnapshot {
    /// Share of total CPU time across all logical processors that was not
    /// idle; 100.0 means every core was fully busy.
    pub cpu_busy_percent: Option<f64>,
    /// Basis of `cpu_busy_percent`: `system_capacity_all_cores`.
    pub cpu_busy_percent_basis: String,
    pub memory_load_percent: Option<f64>,
    pub total_memory_bytes: Option<u64>,
    pub available_memory_bytes: Option<u64>,
}

/// One entry in a snapshot's process summary.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct DiskSnapshotEntry {
    pub root: String,
    pub kind: String,
    pub total_bytes: Option<u64>,
    pub free_bytes: Option<u64>,
}

/// A detected development tool.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct DevTool {
    pub name: String,
    pub found: bool,
    pub path: Option<String>,
    pub version: Option<String>,
    /// Why `version` is `None` when the tool was found but the `--version`
    /// probe failed (timeout, non-zero exit, no output, …). Absent when the
    /// tool was not found or the version probe succeeded.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version_reason: Option<String>,
}

/// Result of `dev_environment`: structured info for coding agents.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct DevEnvironment {
    pub tools: Vec<DevTool>,
    /// Processes on well-known developer ports, e.g. Node dev servers.
    pub development_servers: Vec<DevServerInfo>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct DevServerInfo {
    pub port: u16,
    pub pid: Option<u32>,
    pub process_name: Option<String>,
}

/// Known developer tools probed by `dev_environment`.
pub const KNOWN_DEV_TOOLS: &[&str] = &[
    "node", "npm", "pnpm", "yarn", "bun", "python", "python3", "pip", "pip3", "cargo", "rustc",
    "java", "javac", "gradle", "mvn", "docker", "git", "go", "dotnet", "pwsh",
];

/// Well-known development server process names, matched against listening ports.
pub const KNOWN_DEV_SERVER_NAMES: &[&str] = &[
    "node.exe",
    "python.exe",
    "python3.exe",
    "docker-proxy.exe",
    "com.docker.backend.exe",
    "dotnet.exe",
    "java.exe",
    "bun.exe",
    "go.exe",
    "ruby.exe",
    "php.exe",
];

/// Well-known developer port ranges that are cheap to summarize.
pub fn is_development_port(port: u16) -> bool {
    matches!(port, 3000..=3010 | 4000..=4010 | 5000..=5010 | 5173 | 5174 | 8000..=8010 | 8080..=8090 | 9000..=9010 | 9229)
}

pub type ToolMap = BTreeMap<String, DevTool>;

// ---------------------------------------------------------------------------
// Environment & update models (PATH audit, hotfixes, pending reboot)
// ---------------------------------------------------------------------------

/// One audited PATH entry.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PathEntry {
    /// The entry as written (unexpanded).
    pub raw: String,
    /// After `%VAR%` expansion and quote trimming.
    pub expanded: String,
    /// The directory exists.
    pub exists: bool,
    /// Which scope(s) contain this entry (`machine`, `user`, `process`).
    pub scopes: Vec<String>,
}

/// Full PATH audit result returned by `audit_path_env`.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct PathAudit {
    /// Effective process entries in order, with per-entry findings.
    pub process_entries: Vec<PathEntry>,
    /// Machine-scope raw value was readable.
    pub machine_path_available: bool,
    /// User-scope raw value was readable.
    pub user_path_available: bool,
    /// Entry indexes (into `process_entries`) that appear in multiple scopes.
    pub duplicate_indexes: Vec<usize>,
    /// Indexes of empty or whitespace-only entries (`;;`).
    pub empty_indexes: Vec<usize>,
    /// Indexes of entries whose directory does not exist.
    pub missing_indexes: Vec<usize>,
    /// Human-readable summary of issues; empty when the PATH is clean.
    pub issues: Vec<String>,
    pub total_entries: usize,
}

/// One installed quick-fix engineering (QFE) update.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Hotfix {
    /// e.g. `KB5029350`.
    pub hotfix_id: String,
    /// e.g. `Update`, `Security Update`.
    pub description: Option<String>,
    /// As reported by WMI (locale-formatted date string), when available.
    pub installed_on: Option<String>,
}

/// Update posture returned by `system_update_status`.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct UpdateStatus {
    pub reboot_pending: bool,
    /// Which of the three standard pending-reboot markers fired.
    pub reboot_signals: Vec<String>,
    /// Most recent hotfixes first, bounded by the requested cap.
    pub hotfixes: Vec<Hotfix>,
    pub total_hotfixes_reported: usize,
    /// Reads that failed, with reasons; the rest of the report stays valid.
    pub unavailable: Vec<String>,
}

#[cfg(test)]
mod env_update_tests {
    use super::*;

    #[test]
    fn path_audit_and_update_status_round_trip() {
        let audit = PathAudit {
            process_entries: vec![PathEntry {
                raw: "%SystemRoot%\\System32".into(),
                expanded: "C:\\Windows\\System32".into(),
                exists: true,
                scopes: vec!["machine".into(), "process".into()],
            }],
            machine_path_available: true,
            user_path_available: false,
            duplicate_indexes: vec![0],
            empty_indexes: Vec::new(),
            missing_indexes: Vec::new(),
            issues: vec!["1 duplicate entries across scopes (first shadows later ones)".into()],
            total_entries: 1,
        };
        let json = serde_json::to_string(&audit).unwrap();
        assert_eq!(serde_json::from_str::<PathAudit>(&json).unwrap(), audit);

        let status = UpdateStatus {
            reboot_pending: true,
            reboot_signals: vec!["windows_update_reboot_required".into()],
            hotfixes: vec![Hotfix {
                hotfix_id: "KB1234567".into(),
                description: Some("Update".into()),
                installed_on: Some("8/12/2026".into()),
            }],
            total_hotfixes_reported: 1,
            unavailable: Vec::new(),
        };
        let json = serde_json::to_string(&status).unwrap();
        assert_eq!(serde_json::from_str::<UpdateStatus>(&json).unwrap(), status);
    }
}
