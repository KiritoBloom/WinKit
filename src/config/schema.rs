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
    pub chrome: ChromeConfig,
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
        Self {
            enabled: vec!["windows".to_string(), "chrome".to_string()],
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ToolsConfig {
    /// Tool names to disable, e.g. `["find_large_files"]`.
    pub disabled: Vec<String>,
    /// Active tool profile: `core`, `developer`, `browser`, or `full`.
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

/// Resource limits applied by tools (§49).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct LimitsConfig {
    pub max_processes: usize,
    pub max_network_results: usize,
    pub max_storage_results: usize,
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
            max_storage_results: 200,
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

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ChromeConfig {
    /// Timeout for connecting to the browser inspection endpoint (ms).
    pub connection_timeout_ms: u64,
    /// Absolute deadline for one full Chrome discovery pass (ms). The pass
    /// derives a single deadline from this and gives every probe only the
    /// remaining budget, so discovery stays bounded even when the registry,
    /// process snapshot, or an endpoint probe stalls.
    pub discovery_timeout_ms: u64,
    /// Timeout for a full Chrome operation such as `chrome_diagnose_tab` (ms).
    pub operation_timeout_ms: u64,
    /// How long to observe network/runtime activity for one tab (ms).
    pub observation_window_ms: u64,
    /// Gap between the two performance/memory samples (ms).
    pub sample_interval_ms: u64,
    /// Cap on a single Chrome-related response payload (bytes).
    pub max_payload_bytes: usize,
    /// Maximum number of tabs returned by tab-listing tools.
    pub max_tabs: usize,
    /// Probe this fixed port as a last-resort endpoint discovery fallback.
    pub fallback_port: u16,
    /// Automatically detect and connect, or only report availability.
    pub auto_connect: bool,
    /// Gap between consecutive samples of the tab trend tool (ms).
    pub trend_sample_interval_ms: u64,
    /// Upper bound the trend tool accepts for its observation window (ms).
    pub trend_max_ms: u64,
    /// Managed (WinKit-spawned) Chrome sessions (`[chrome.managed]`).
    pub managed: ChromeManagedConfig,
}

impl Default for ChromeConfig {
    fn default() -> Self {
        Self {
            connection_timeout_ms: 5_000,
            discovery_timeout_ms: 1_500,
            operation_timeout_ms: 25_000,
            observation_window_ms: 3_000,
            sample_interval_ms: 500,
            max_payload_bytes: 500_000,
            max_tabs: 200,
            fallback_port: 9222,
            auto_connect: true,
            trend_sample_interval_ms: 2_000,
            trend_max_ms: 30_000,
            managed: ChromeManagedConfig::default(),
        }
    }
}

/// Managed (WinKit-spawned) Chrome configuration.
///
/// Every field defaults to the safest value. Lifecycle tools stay disabled
/// until `enabled = true` is set explicitly, and even then every action is
/// permission-gated and every URL/path is validated.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ChromeManagedConfig {
    /// Master switch for managed-session lifecycle tools.
    pub enabled: bool,
    /// Root directory for WinKit-owned profiles. Empty = system temp dir
    /// under a `winkit-managed` subdirectory. Cleanup only ever deletes
    /// canonical paths under this root.
    pub profile_root: String,
    /// Timeout for Chrome startup + DevTools endpoint readiness (ms).
    pub startup_timeout_ms: u64,
    /// Remove the owned profile directory when a session closes.
    pub cleanup_on_close: bool,
    /// Allow navigation to non-localhost hosts from a managed session.
    pub allow_external_urls: bool,
    /// Whether spawned sessions are headless by default.
    pub default_headless: bool,
    /// Maximum number of concurrent WinKit-owned sessions.
    pub max_sessions: usize,
    /// Maximum number of browser targets reported per session.
    pub max_targets: usize,
    /// Cap on the page-summary text WinKit returns (characters).
    pub max_summary_chars: usize,
    /// Cap on the larger screenshot dimension (pixels).
    pub max_screenshot_dimension: usize,
    /// Cap on a serialized screenshot payload (bytes).
    pub max_screenshot_bytes: usize,
}

impl Default for ChromeManagedConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            profile_root: String::new(),
            startup_timeout_ms: 10_000,
            cleanup_on_close: true,
            allow_external_urls: false,
            default_headless: false,
            max_sessions: 2,
            max_targets: 50,
            max_summary_chars: 8_000,
            max_screenshot_dimension: 1_280,
            max_screenshot_bytes: 512 * 1024,
        }
    }
}

/// Deterministic thresholds used by the diagnostic engine (§34).
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
        }
    }
}

/// Thresholds for the machine-wide `system_health` tool (§76).
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
        // Managed Chrome is off by default.
        assert!(!c.chrome.managed.enabled);
        assert_eq!(c.chrome.managed.max_sessions, 2);
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
    fn managed_chrome_section_parses() {
        let text = r#"
            [chrome.managed]
            enabled = true
            profile_root = "C:\\winkit-profiles"
            startup_timeout_ms = 15000
            cleanup_on_close = true
            allow_external_urls = false
            default_headless = true
            max_sessions = 1
        "#;
        let c: Config = toml::from_str(text).unwrap();
        assert!(c.chrome.managed.enabled);
        assert_eq!(c.chrome.managed.profile_root, "C:\\winkit-profiles");
        assert_eq!(c.chrome.managed.startup_timeout_ms, 15_000);
        assert!(c.chrome.managed.default_headless);
        assert_eq!(c.chrome.managed.max_sessions, 1);
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
    fn tools_profile_parses_and_rejects_unknown() {
        let c: Config = toml::from_str("[tools]\nprofile = \"browser\"").unwrap();
        assert_eq!(c.tools.profile, "browser");
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
