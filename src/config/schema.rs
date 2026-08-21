//! WinKit configuration model (`winkit.toml`).
//!
//! Every field has a sensible default, so a missing file is fine. Unknown
//! keys are rejected loudly so typos surface immediately instead of being
//! silently ignored.

use serde::{Deserialize, Serialize};

/// Root configuration.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    pub server: ServerConfig,
    pub permissions: PermissionConfig,
    pub providers: ProvidersConfig,
    pub tools: ToolsConfig,
    pub workspaces: WorkspacesConfig,
    pub web: WebConfig,
    pub limits: LimitsConfig,
    pub hardware: HardwareConfig,
    pub trends: TrendsConfig,
    pub diagnostics: DiagnosticsConfig,
    pub health: HealthConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ServerConfig {
    /// `error`, `warn`, `info`, `debug`, or `trace`.
    pub log_level: String,
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            log_level: "info".to_string(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct PermissionConfig {
    /// `safe`, `read_only`, `approval`, or `unrestricted`.
    pub mode: String,
}

impl Default for PermissionConfig {
    fn default() -> Self {
        Self {
            mode: "read_only".to_string(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ProvidersConfig {
    /// Provider IDs that are active. Empty means "all built-in providers".
    pub enabled: Vec<String>,
}

impl Default for ProvidersConfig {
    fn default() -> Self {
        // The windows provider is the only built-in provider.
        Self {
            enabled: vec!["windows".to_string()],
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ToolsConfig {
    /// Tool names to disable, e.g. `["snapshot"]`.
    pub disabled: Vec<String>,
    /// Active tool profile: `core`, `developer`, or `full`.
    /// `tools/list` exposes only the tools of the effective profile.
    pub profile: String,
}

impl Default for ToolsConfig {
    fn default() -> Self {
        Self {
            disabled: Vec::new(),
            profile: "developer".to_string(),
        }
    }
}

/// Workspace inspection policy: which paths WinKit may scan and how deep.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct WorkspacesConfig {
    /// Paths that may be scanned. Empty means "any path allowed".
    pub allow_roots: Vec<String>,
    /// Paths that are never scanned, e.g. personal folders or secrets dirs.
    /// A path is rejected when it equals or is contained under a deny root.
    pub deny_roots: Vec<String>,
    /// Maximum directory depth scanned below the workspace root.
    pub max_depth: u32,
    /// Maximum number of files/directories examined in one scan.
    pub max_files: usize,
}

impl Default for WorkspacesConfig {
    fn default() -> Self {
        Self {
            allow_roots: Vec::new(),
            deny_roots: Vec::new(),
            max_depth: 8,
            max_files: 2000,
        }
    }
}

/// URL and HTTP policy for local web tooling.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct WebConfig {
    /// Allow external (non-loopback, non-localhost) hosts. Default `false`.
    pub allow_external_urls: bool,
    /// Explicitly trusted development hosts, e.g. `["dev.myapp.test"]`.
    /// Hosts here are reachable even when `allow_external_urls` is false.
    pub dev_hosts: Vec<String>,
    /// Permit probing `https://` local endpoints (self-signed TLS is
    /// reported, never validated against the system store).
    pub local_tls_allowed: bool,
    /// Cap on an HTTP response body read by WinKit (bytes).
    pub max_http_bytes: usize,
    /// Absolute deadline for one HTTP probe (ms).
    pub max_http_ms: u64,
    /// Maximum redirect hops followed during a probe.
    pub max_redirects: usize,
}

impl Default for WebConfig {
    fn default() -> Self {
        Self {
            allow_external_urls: false,
            dev_hosts: Vec::new(),
            local_tls_allowed: true,
            max_http_bytes: 256 * 1024,
            max_http_ms: 5_000,
            max_redirects: 10,
        }
    }
}

/// Bounded local trend sampling (`system_health_trend`).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct TrendsConfig {
    /// Upper bound a caller-requested trend window may be (ms).
    pub max_window_ms: u64,
    /// Default polling interval when the caller does not set one (ms).
    pub default_interval_ms: u64,
    /// Maximum number of samples per trend.
    pub max_samples: usize,
}

impl Default for TrendsConfig {
    fn default() -> Self {
        Self {
            max_window_ms: 120_000,
            default_interval_ms: 5_000,
            max_samples: 24,
        }
    }
}

/// Resource limits applied by tools.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct LimitsConfig {
    pub max_processes: usize,
    pub max_network_results: usize,
    pub max_events: usize,
    pub max_services: usize,
    pub max_windows: usize,
    pub max_tabs: usize,
    pub max_snapshot_processes: usize,
    pub max_find_depth: u32,
    /// Cap on any single serialized MCP response payload (bytes).
    pub max_payload_bytes: usize,
    /// Default timeout for a single tool operation (ms).
    pub operation_timeout_ms: u64,
    /// Maximum diagnostics of any kind running concurrently.
    pub max_concurrent_diagnostics: usize,
}

impl Default for LimitsConfig {
    fn default() -> Self {
        Self {
            max_processes: 500,
            max_network_results: 1000,
            max_events: 200,
            max_services: 500,
            max_windows: 500,
            max_tabs: 200,
            max_snapshot_processes: 25,
            max_find_depth: 8,
            max_payload_bytes: 2_000_000,
            operation_timeout_ms: 30_000,
            max_concurrent_diagnostics: 8,
        }
    }
}

/// Hardware telemetry policy.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct HardwareConfig {
    /// Master switch for hardware sensor collection (thermal zones, CPU
    /// frequency, battery health, storage health). Default `true`; every
    /// reading is still reported as explicitly unavailable when the platform
    /// has no supported path for it.
    pub sensors_enabled: bool,
    /// Master switch for Wi-Fi scanning (`wifi_scan`). Scanning enumerates
    /// nearby networks and is disabled by default; when disabled the tool
    /// returns `unavailable` with a reason instead of an empty list.
    pub wifi_scan_enabled: bool,
    /// Whether storage health probes may issue ATA S.M.A.R.T. pass-through
    /// IOCTLs. NVMe log-page reads are always allowed; ATA pass-through is
    /// historically more variable across drivers, so it defaults off.
    pub ata_smart_enabled: bool,
    /// Timeout for one hardware provider probe (thermal zone, NVMe SMART,
    /// battery capacity), in milliseconds. Each probe is individually bounded
    /// so a stalled driver cannot hang a snapshot.
    pub probe_timeout_ms: u64,
}

impl Default for HardwareConfig {
    fn default() -> Self {
        Self {
            sensors_enabled: true,
            wifi_scan_enabled: false,
            ata_smart_enabled: false,
            probe_timeout_ms: 3_000,
        }
    }
}

/// Deterministic thresholds used by the diagnostic engine.
/// These are heuristics, documented in `docs/diagnostics.md`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct DiagnosticsConfig {
    /// Aggregate process CPU percent of total system CPU capacity (100% =
    /// all logical processors fully busy) that counts as high.
    pub high_cpu_percent: f64,
    /// JS heap size (bytes) that counts as high memory.
    pub high_heap_bytes: u64,
    /// Heap growth rate (bytes/second) that counts as rapid growth.
    pub heap_growth_bytes_per_second: u64,
    /// Sustained heap growth rate (bytes/second), when samples trend upward
    /// repeatedly across a time series.
    pub sustained_growth_bytes_per_second: u64,
    /// Main-thread script duration (ms) within one observation window that
    /// counts as heavy JS activity.
    pub high_script_ms: f64,
    /// Cumulative long-task time (ms) within one window that counts as many
    /// long tasks.
    pub long_task_ms: f64,
    /// Failed requests at or above this count trigger a network signal.
    pub failed_request_threshold: usize,
    /// Failed/total ratio at or above this triggers a network signal.
    pub failed_request_ratio: f64,
    /// Average response time (ms) that counts as high latency.
    pub high_latency_ms: f64,
    /// p95 response time (ms) that counts as high latency.
    pub high_p95_ms: f64,
    /// Transferred bytes within one window that counts as heavy network.
    pub high_network_bytes: u64,
    /// Console errors + exceptions at or above this count is a runtime signal.
    pub runtime_error_threshold: usize,
    /// DOM nodes at or above this count contributes to the memory signal.
    pub high_dom_nodes: u64,
    /// System available-memory decrease (bytes/second) that counts as
    /// runaway memory growth in `system_diagnose`.
    pub system_memory_growth_bytes_per_second: u64,
    /// CPU package temperature (C) that counts as thermal pressure.
    pub high_cpu_temperature_c: f64,
    /// CPU package temperature (C) at which throttling is considered likely.
    pub throttle_cpu_temperature_c: f64,
    /// GPU temperature (C) that counts as thermal pressure.
    pub high_gpu_temperature_c: f64,
    /// Current CPU frequency at or below this fraction of base clock suggests
    /// clock reduction (throttling or power saving), 0.0-1.0.
    pub cpu_frequency_reduction_ratio: f64,
    /// Percent of time a physical disk is busy that counts as storage
    /// contention.
    pub high_storage_busy_percent: f64,
    /// Average disk queue depth that counts as storage contention.
    pub high_storage_queue_depth: f64,
    /// NVMe/ATA device health is `warning` below this threshold.
    pub storage_health_warning_percent: f64,
    /// NVMe percentage used at or above this is `warning`.
    pub storage_used_warning_percent: u8,
    /// Wi-Fi signal at or below this percent is weak.
    pub weak_wifi_signal_percent: u8,
    /// Wi-Fi link speed at or below this (Mbps) is low.
    pub low_wifi_link_speed_mbps: f64,
    /// Battery health (full charge / design capacity) at or below this
    /// percent is degraded.
    pub low_battery_health_percent: f64,
}

impl Default for DiagnosticsConfig {
    fn default() -> Self {
        Self {
            high_cpu_percent: 30.0,
            high_heap_bytes: 512 * 1024 * 1024,
            heap_growth_bytes_per_second: 2 * 1024 * 1024,
            sustained_growth_bytes_per_second: 1024 * 1024,
            high_script_ms: 1_500.0,
            long_task_ms: 1_000.0,
            failed_request_threshold: 10,
            failed_request_ratio: 0.1,
            high_latency_ms: 500.0,
            high_p95_ms: 1_500.0,
            high_network_bytes: 10 * 1024 * 1024,
            runtime_error_threshold: 5,
            high_dom_nodes: 50_000,
            system_memory_growth_bytes_per_second: 50 * 1024 * 1024,
            high_cpu_temperature_c: 85.0,
            throttle_cpu_temperature_c: 95.0,
            high_gpu_temperature_c: 90.0,
            cpu_frequency_reduction_ratio: 0.85,
            high_storage_busy_percent: 85.0,
            high_storage_queue_depth: 2.0,
            storage_health_warning_percent: 80.0,
            storage_used_warning_percent: 90,
            weak_wifi_signal_percent: 40,
            low_wifi_link_speed_mbps: 20.0,
            low_battery_health_percent: 60.0,
        }
    }
}

/// Thresholds for the machine-wide `system_health` tool.
/// Heuristics, documented in `docs/tools.md`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct HealthConfig {
    /// Aggregate CPU percent of total system CPU capacity (100% = all
    /// logical processors fully busy) that marks an application group as
    /// `high_cpu`.
    pub high_cpu_percent: f64,
    /// Total working set (bytes) that marks an application group as
    /// `high_memory`.
    pub high_memory_bytes: u64,
    /// Free space at or below this (bytes) marks a drive as dangerously low.
    pub low_disk_free_bytes: u64,
    /// System memory load at or above this percent is `memory_pressure`.
    pub high_memory_load_percent: f64,
    /// Maximum number of application groups returned, by working set.
    pub max_groups: usize,
}

impl Default for HealthConfig {
    fn default() -> Self {
        Self {
            high_cpu_percent: 30.0,
            high_memory_bytes: 2 * 1024 * 1024 * 1024,
            low_disk_free_bytes: 10 * 1024 * 1024 * 1024,
            high_memory_load_percent: 85.0,
            max_groups: 20,
        }
    }
}

impl Config {
    /// Resolve the permission mode, validating the configured name.
    pub fn permission_mode(
        &self,
    ) -> Result<crate::permissions::PermissionMode, crate::errors::WinkitError> {
        crate::permissions::PermissionMode::parse(&self.permissions.mode).ok_or_else(|| {
            crate::errors::WinkitError::invalid_argument(format!(
                "invalid permission mode '{}' (expected safe, read_only, approval, or unrestricted)",
                self.permissions.mode
            ))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_are_sane() {
        let c = Config::default();
        assert_eq!(c.permissions.mode, "read_only");
        assert_eq!(c.limits.max_processes, 500);
        assert!(c.limits.max_payload_bytes > 0);
        // Tool profiles default to `developer`.
        assert_eq!(c.tools.profile, "developer");
        // Hardware telemetry is on, Wi-Fi scanning is off by default.
        assert!(c.hardware.sensors_enabled);
        assert!(!c.hardware.wifi_scan_enabled);
        assert!(c.hardware.probe_timeout_ms > 0);
        // Web policy is loopback-only by default.
        assert!(!c.web.allow_external_urls);
        assert!(c.web.max_http_bytes > 0);
        assert!(c.web.max_http_ms > 0);
        // Workspace scanning is bounded.
        assert!(c.workspaces.max_files > 0);
        assert!(c.workspaces.max_depth > 0);
        assert!(c.trends.max_samples > 0);
        assert!(c.limits.max_concurrent_diagnostics > 0);
    }

    #[test]
    fn web_and_workspaces_sections_parse() {
        let text = r#"
            [web]
            allow_external_urls = false
            dev_hosts = ["dev.myapp.test"]
            max_http_bytes = 1024

            [workspaces]
            deny_roots = ["C:\\Users\\me\\Documents"]
            max_depth = 5
            max_files = 100
        "#;
        let c: Config = toml::from_str(text).unwrap();
        assert_eq!(c.web.dev_hosts, vec!["dev.myapp.test"]);
        assert_eq!(c.web.max_http_bytes, 1024);
        assert_eq!(c.workspaces.deny_roots.len(), 1);
        assert_eq!(c.workspaces.max_depth, 5);
        assert_eq!(c.workspaces.max_files, 100);
    }

    #[test]
    fn hardware_section_parses() {
        let text = r#"
            [hardware]
            sensors_enabled = true
            wifi_scan_enabled = true
            ata_smart_enabled = true
            probe_timeout_ms = 1500
        "#;
        let c: Config = toml::from_str(text).unwrap();
        assert!(c.hardware.sensors_enabled);
        assert!(c.hardware.wifi_scan_enabled);
        assert!(c.hardware.ata_smart_enabled);
        assert_eq!(c.hardware.probe_timeout_ms, 1_500);
    }

    #[test]
    fn tools_profile_parses_and_rejects_unknown() {
        let c: Config = toml::from_str("[tools]\nprofile = \"full\"").unwrap();
        assert_eq!(c.tools.profile, "full");
        let bad: Result<Config, _> = toml::from_str("[tools]\nprofile = \"nonsense\"");
        // Parse succeeds; the profile name is validated when resolved.
        assert!(bad.is_ok());
        let unknown: Result<Config, _> = toml::from_str("[bogus_section]\nx = 1");
        assert!(unknown.is_err());
    }

    #[test]
    fn unknown_keys_are_rejected() {
        let bad = r#"
            [server]
            log_level = "info"
            [bogus_section]
            x = 1
        "#;
        let err = toml::from_str::<Config>(bad);
        assert!(err.is_err());
    }
}
