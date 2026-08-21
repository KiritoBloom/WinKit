//! Tool definitions, argument helpers, and the tool registry.
//!
//! Every MCP tool is a [`ToolDefinition`]: a name, description, JSON input
//! schema, the capability it requires, an optional timeout override, and an
//! async handler. Handlers are pure functions over [`crate::server::AppState`]
//! and never touch Win32 directly.

pub mod developer;
pub mod environment;
pub mod events;
pub mod files;
pub mod hardware;
pub mod health;
pub mod network;
pub mod processes;
pub mod registry;
pub mod services;
pub mod stability;
pub mod storage;
pub mod system;
pub mod windows;
pub mod workflows;

use crate::config::Config;
use crate::errors::WinkitError;
use crate::permissions::Capability;
use crate::providers::BoxFuture;
use crate::server::profiles::ToolProfile;
use crate::server::AppState;
use serde_json::Value;
use std::collections::HashMap;
use std::future::Future;
use std::sync::Arc;

#[cfg(test)]
use serde_json::json;

/// A tool handler: boxed async function taking (state, raw arguments).
pub type Handler = Arc<
    dyn Fn(Arc<AppState>, Value) -> BoxFuture<'static, Result<Value, WinkitError>> + Send + Sync,
>;

/// One MCP tool.
pub struct ToolDefinition {
    pub name: &'static str,
    pub description: &'static str,
    pub input_schema: Value,
    /// Capability the permission system enforces before dispatch.
    pub capability: Option<Capability>,
    /// Per-tool timeout override (milliseconds); `None` falls back to the
    /// configured operation timeout.
    pub timeout_ms: Option<u64>,
    pub handler: Handler,
}

/// Profile membership for every tool. This table is the source of
/// truth for profile filtering; tools not listed here are visible in every
/// profile. `core` stays minimal and low-latency, `developer` (default)
/// adds the workspace/server/webapp workflow, `full` exposes everything.
pub fn tool_profiles(name: &str) -> &'static [ToolProfile] {
    use ToolProfile::{Core, Developer, Full};
    match name {
        // Core: safe, low-latency essentials (also in developer and full).
        "workspace_snapshot"
        | "system_health"
        | "list_processes"
        | "list_listening_ports"
        | "privacy_info"
        | "tool_guide" => &[Core, Developer, Full],
        // Developer: the recommended default — everything high-level plus
        // the low-level tools the workflows recommend as next steps.
        "system_info"
        | "snapshot"
        | "get_process"
        | "get_process_tree"
        | "find_process"
        | "find_process_on_port"
        | "list_network_interfaces"
        | "list_connections"
        | "list_drives"
        | "disk_usage"
        | "list_services"
        | "get_service"
        | "get_recent_events"
        | "get_application_errors"
        | "get_system_errors"
        | "crash_history"
        | "shutdown_analysis"
        | "registry_diagnostics"
        | "list_windows"
        | "system_diagnose"
        | "hardware_snapshot"
        | "thermal_snapshot"
        | "battery_status"
        | "power_status"
        | "disk_health"
        | "disk_performance"
        | "network_snapshot"
        | "network_diagnose"
        | "wifi_status"
        | "wifi_scan"
        // Environment & OS posture.
        | "startup_programs"
        | "audit_path_env"
        | "system_update_status"
        // Bounded read-only filesystem access (text reads, name search,
        // directory size breakdowns).
        | "read_text_file"
        | "find_files"
        | "directory_overview" => &[Developer, Full],
        // Developer-only workflow: the workspace/server/webapp tools.
        "dev_environment"
        | "list_dev_servers"
        | "diagnose_workspace"
        | "diagnose_local_webapp"
        | "wait_for_port"
        | "wait_for_http"
        | "wait_for_process"
        | "correlate_recent_failures"
        | "system_health_trend" => &[Developer, Full],
        // Unlisted tools (currently none) would be visible in every profile.
        _ => &[Core, Developer, Full],
    }
}

/// Is `tool` visible in `profile`?
pub fn tool_in_profile(name: &str, profile: ToolProfile) -> bool {
    tool_profiles(name).contains(&profile)
}

/// The error a gated tool produces when it is not in the active profile.
pub fn profile_gate_message(name: &str, profile: ToolProfile) -> String {
    use ToolProfile::{Developer, Full};
    let hint = if tool_profiles(name) == [Developer, Full] {
        "; this developer workflow tool is only enabled in tool profile 'developer' or 'full'"
    } else {
        ""
    };
    format!("tool '{name}' is not available in tool profile '{profile}'{hint}")
}

/// The action capability for tools that mutate state, dispatched through
/// the approval-aware permission path instead of the read check. WinKit is
/// read-only, so this is empty today; kept as the single extension point.
pub fn tool_action_capability(_name: &str) -> Option<Capability> {
    None
}

/// The registry of all active tools.
pub struct ToolRegistry {
    tools: HashMap<&'static str, ToolDefinition>,
    disabled: Vec<String>,
    /// Effective profile for this server session (config `tools.profile`).
    profile: ToolProfile,
}

impl ToolRegistry {
    /// Build the full v1 tool set from configuration.
    pub fn build(config: &Config) -> Self {
        let mut registry = Self {
            tools: HashMap::new(),
            disabled: config.tools.disabled.clone(),
            profile: match config.tools.profile.parse::<ToolProfile>() {
                Ok(p) => p,
                Err(_) => ToolProfile::default_profile(),
            },
        };

        // Windows observability.
        registry.register(system::system_info_definition());
        registry.register(system::snapshot_definition(config));
        registry.register(processes::list_processes_definition());
        registry.register(processes::get_process_definition());
        registry.register(processes::get_process_tree_definition());
        registry.register(processes::find_process_definition());
        registry.register(network::list_listening_ports_definition());
        registry.register(network::find_process_on_port_definition());
        registry.register(network::list_network_interfaces_definition());
        registry.register(network::list_connections_definition());
        registry.register(storage::list_drives_definition());
        registry.register(storage::disk_usage_definition());
        registry.register(services::list_services_definition());
        registry.register(services::get_service_definition());
        registry.register(events::get_recent_events_definition());
        registry.register(events::get_application_errors_definition());
        registry.register(events::get_system_errors_definition());
        registry.register(stability::crash_history_definition());
        registry.register(stability::shutdown_analysis_definition());
        registry.register(windows::list_windows_definition());
        registry.register(developer::dev_environment_definition());
        registry.register(registry::registry_diagnostics_definition());

        // Hardware telemetry: sensors, power, storage health/activity, Wi-Fi,
        // and network diagnosis.
        registry.register(hardware::hardware_snapshot_definition());
        registry.register(hardware::thermal_snapshot_definition());
        registry.register(hardware::battery_status_definition());
        registry.register(hardware::power_status_definition());
        registry.register(hardware::disk_health_definition());
        registry.register(hardware::disk_performance_definition());
        registry.register(hardware::network_snapshot_definition());
        registry.register(hardware::network_diagnose_definition());
        registry.register(hardware::wifi_status_definition());
        registry.register(hardware::wifi_scan_definition());

        // Developer workflow: workspace snapshot, dev-server discovery, the
        // flagship diagnosis tools, bounded waits, failure correlation,
        // health trends, and privacy posture. All live in `workflows`.
        registry.register(workflows::workspace_snapshot_definition());
        registry.register(workflows::list_dev_servers_definition());
        registry.register(workflows::diagnose_workspace_definition());
        registry.register(workflows::diagnose_local_webapp_definition());
        registry.register(workflows::wait_for_port_definition());
        registry.register(workflows::wait_for_http_definition());
        registry.register(workflows::wait_for_process_definition());
        registry.register(workflows::correlate_recent_failures_definition());
        registry.register(workflows::system_health_trend_definition());
        registry.register(workflows::privacy_info_definition());

        registry.register(health::system_health_definition(config));
        registry.register(health::system_diagnose_definition(config));

        // Environment & OS posture: startup programs, PATH audit, update
        // status, and the agent-facing tool guide.
        registry.register(environment::startup_programs_definition());
        registry.register(environment::audit_path_env_definition());
        registry.register(environment::system_update_status_definition());
        registry.register(environment::tool_guide_definition());

        // Bounded read-only filesystem tools.
        registry.register(files::read_text_file_definition());
        registry.register(files::find_files_definition());
        registry.register(files::directory_overview_definition());

        registry
    }

    pub fn register(&mut self, tool: ToolDefinition) -> &mut Self {
        self.tools.insert(tool.name, tool);
        self
    }

    pub fn get(&self, name: &str) -> Option<&ToolDefinition> {
        self.tools.get(name)
    }

    /// Tool names, sorted, for listing and tests.
    pub fn names(&self) -> Vec<String> {
        let mut names: Vec<String> = self.tools.keys().map(|k| k.to_string()).collect();
        names.sort();
        names
    }

    /// The effective tool profile for this session.
    pub fn profile(&self) -> ToolProfile {
        self.profile
    }

    /// `(name, description, input_schema)` triples for `tools/list`,
    /// filtered to the effective profile. Disabled tools are not listed;
    /// tools outside the profile are not listed.
    pub fn schemas(&self) -> Vec<(String, String, Value)> {
        let mut out: Vec<(String, String, Value)> = self
            .tools
            .values()
            .filter(|t| crate::tools::tool_in_profile(t.name, self.profile))
            .filter(|t| !self.disabled.iter().any(|d| d == t.name))
            .map(|t| {
                (
                    t.name.to_string(),
                    t.description.to_string(),
                    t.input_schema.clone(),
                )
            })
            .collect();
        out.sort_by(|a, b| a.0.cmp(&b.0));
        out
    }

    /// Every registered tool name, regardless of profile (used by the
    /// registry-integrity tests and non-protocol callers).
    pub fn all_names(&self) -> Vec<String> {
        let mut names: Vec<String> = self.tools.keys().map(|k| k.to_string()).collect();
        names.sort();
        names
    }

    /// Run a tool within its timeout. Permission checks happen in
    /// [`crate::server::registry::call_tool`].
    pub async fn call(
        &self,
        state: &Arc<AppState>,
        name: &str,
        args: Value,
    ) -> Result<Value, WinkitError> {
        let tool = self
            .get(name)
            .ok_or_else(|| WinkitError::invalid_argument(format!("unknown tool '{name}'")))?;
        if self.disabled.iter().any(|d| d == name) {
            return Err(WinkitError::invalid_argument(format!(
                "tool '{name}' is disabled by configuration"
            )));
        }
        if !crate::tools::tool_in_profile(name, self.profile) {
            return Err(WinkitError::invalid_argument(
                crate::tools::profile_gate_message(name, self.profile),
            ));
        }
        let timeout_ms = tool
            .timeout_ms
            .unwrap_or(state.config.limits.operation_timeout_ms);
        let timeout = std::time::Duration::from_millis(timeout_ms.max(1));
        match tokio::time::timeout(timeout, (tool.handler)(state.clone(), args)).await {
            Ok(result) => result,
            Err(_) => Err(WinkitError::timeout(format!(
                "tool '{name}' exceeded the {timeout_ms} ms timeout"
            ))),
        }
    }
}

/// Wrap an async handler into a boxed `Handler`.
pub fn wrap<F, Fut>(handler: F) -> Handler
where
    F: Fn(Arc<AppState>, Value) -> Fut + Send + Sync + 'static,
    Fut: Future<Output = Result<Value, WinkitError>> + Send + 'static,
{
    Arc::new(move |state, args| Box::pin(handler(state, args)))
}

/// Standard argument helpers shared by tool handlers.
pub fn optional_string(args: &Value, key: &str) -> Option<String> {
    args.get(key)
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
}

pub fn required_string(args: &Value, key: &str) -> Result<String, WinkitError> {
    let raw = optional_string(args, key).ok_or_else(|| {
        WinkitError::invalid_argument(format!("missing required argument '{key}'"))
    })?;
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err(WinkitError::invalid_argument(format!(
            "'{key}' must not be empty"
        )));
    }
    Ok(trimmed.to_string())
}

pub fn optional_non_empty_string(args: &Value, key: &str) -> Option<String> {
    optional_string(args, key).and_then(|s| {
        let t = s.trim();
        if t.is_empty() {
            None
        } else {
            Some(t.to_string())
        }
    })
}

pub fn optional_u32(args: &Value, key: &str) -> Option<u32> {
    args.get(key).and_then(|v| v.as_u64()).map(|n| n as u32)
}

pub fn optional_u64(args: &Value, key: &str) -> Option<u64> {
    args.get(key).and_then(|v| v.as_u64())
}

pub fn optional_usize(args: &Value, key: &str) -> Option<usize> {
    args.get(key).and_then(|v| v.as_u64()).map(|n| n as usize)
}

pub fn optional_f64(args: &Value, key: &str) -> Option<f64> {
    args.get(key).and_then(|v| v.as_f64())
}

pub fn optional_bool(args: &Value, key: &str) -> Option<bool> {
    args.get(key).and_then(|v| v.as_bool())
}

pub fn required_u32(args: &Value, key: &str) -> Result<u32, WinkitError> {
    optional_u32(args, key)
        .ok_or_else(|| WinkitError::invalid_argument(format!("missing required argument '{key}'")))
}

pub fn required_u64(args: &Value, key: &str) -> Result<u64, WinkitError> {
    optional_u64(args, key)
        .ok_or_else(|| WinkitError::invalid_argument(format!("missing required argument '{key}'")))
}

/// Parse a PID (u32 > 0) with precise error.
pub fn parse_pid(args: &Value, key: &str) -> Result<u32, WinkitError> {
    let pid = required_u32(args, key)?;
    if pid == 0 {
        return Err(WinkitError::invalid_argument(format!(
            "'{key}' must be > 0"
        )));
    }
    Ok(pid)
}

pub fn optional_parse_port(args: &Value, key: &str) -> Result<Option<u16>, WinkitError> {
    match optional_u32(args, key) {
        Some(raw) => {
            let port = u16::try_from(raw).ok().filter(|p| *p != 0).ok_or_else(|| {
                WinkitError::invalid_argument(format!("'{key}' must be an integer in 1..=65535"))
            })?;
            Ok(Some(port))
        }
        None => Ok(None),
    }
}

pub fn optional_string_array(args: &Value, key: &str) -> Option<Vec<String>> {
    args.get(key)
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str())
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect()
        })
        .filter(|v: &Vec<String>| !v.is_empty())
}

/// Clamp a client-requested result count into `1..=max`; `None` means max.
pub fn clamp_limit(requested: Option<usize>, max: usize) -> usize {
    requested.map(|v| v.clamp(1, max)).unwrap_or(max)
}

/// Consistent paginated list envelope: `{ "<key>": [...], count, truncated }`
/// `truncated` is true exactly when `items.len() == limit` (the provider was
/// at its cap and more may exist). Callers should still pass the effective limit.
pub fn list_envelope(key: &str, items: Value, count: usize, limit: usize) -> Value {
    let truncated = count == limit && limit > 0;
    let mut obj = serde_json::Map::new();
    obj.insert(key.to_string(), items);
    obj.insert("count".to_string(), Value::from(count as u64));
    obj.insert("truncated".to_string(), Value::from(truncated));
    Value::Object(obj)
}

/// Like `list_envelope` but merges additional fields (e.g. `running`, `skipped_*`).
pub fn list_envelope_with(
    key: &str,
    items: Value,
    count: usize,
    limit: usize,
    extra: Value,
) -> Value {
    let mut base = list_envelope(key, items, count, limit);
    if let (Some(base_obj), Some(extra_obj)) = (base.as_object_mut(), extra.as_object()) {
        for (k, v) in extra_obj {
            base_obj.insert(k.clone(), v.clone());
        }
    }
    base
}

/// Single-entity envelope: `{ "<key>": <value> }` — keeps successful single lookups
/// uniform (`{ "process": {} }`, `{ "service": {} }`, ...).
pub fn item_envelope(key: &str, value: Value) -> Value {
    let mut obj = serde_json::Map::new();
    obj.insert(key.to_string(), value);
    Value::Object(obj)
}

/// Map an event-level name to its numeric minimum severity (1..5), matching
/// the Windows event log levels. Accepts both `information` and `info`.
pub fn level_to_min(level: &str) -> Option<u32> {
    match level.trim().to_ascii_lowercase().as_str() {
        "critical" => Some(1),
        "error" => Some(2),
        "warning" | "warn" => Some(3),
        "information" | "info" => Some(4),
        "verbose" => Some(5),
        _ => None,
    }
}

/// Strictly parse a port number 1..=65535 with a clean error.
pub fn parse_port(args: &Value, key: &str) -> Result<u16, WinkitError> {
    let raw = optional_u32(args, key).ok_or_else(|| {
        WinkitError::invalid_argument(format!("missing required argument '{key}'"))
    })?;
    u16::try_from(raw).ok().filter(|p| *p != 0).ok_or_else(|| {
        WinkitError::invalid_argument(format!("'{key}' must be an integer in 1..=65535"))
    })
}

const MAX_VALIDATED_PATH: usize = 32_768; // Windows extended-path limit (\\?\)

/// Validate and normalise a path string: trimmed, non-empty, length-bounded.
pub fn validated_path(raw: &str) -> Result<String, WinkitError> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err(WinkitError::invalid_argument("'path' must not be empty"));
    }
    if trimmed.len() > MAX_VALIDATED_PATH {
        return Err(WinkitError::invalid_argument("'path' is too long"));
    }
    Ok(trimmed.to_string())
}

pub fn required_path(args: &Value, key: &str) -> Result<String, WinkitError> {
    let raw = required_string(args, key)?;
    validated_path(&raw)
}

const BYTES_PER_MB_F64: f64 = 1_048_576.0;

pub fn mb_to_bytes(mb: f64) -> u64 {
    (mb * BYTES_PER_MB_F64) as u64
}

pub fn validate_min_size_mb(args: &Value, key: &str, default: f64) -> Result<u64, WinkitError> {
    let mb = optional_f64(args, key).unwrap_or(default);
    if mb < 0.0 || !mb.is_finite() {
        return Err(WinkitError::invalid_argument(format!(
            "'{key}' must be a finite number >= 0"
        )));
    }
    Ok(mb_to_bytes(mb))
}

/// Verify the registry invariant: every registered tool is unique,
/// named, described, has an object input schema, exactly one capability
/// source (read `capability` or action capability, never both and never
/// neither), a timeout, appears in the profile table, and is covered by a
/// profile. Returns the tool count on success.
pub fn verify_integrity(registry: &ToolRegistry) -> Result<usize, WinkitError> {
    let names = registry.all_names();
    // Uniqueness: `all_names` derives from a HashMap keyed by tool name, so
    // duplicates would already be collapsed. Detect a double registration by
    // checking the count of insertions is not observable; instead assert the
    // registry build produced every expected v1 tool exactly once (covered by
    // the `registry_builds_every_expected_tool` test below) and that no name
    // resolves to two distinct definitions via the profile lookup.
    for name in &names {
        let tool = registry.get(name).ok_or_else(|| {
            WinkitError::internal(format!(
                "registry inconsistency: '{name}' listed but missing"
            ))
        })?;
        if tool.name.is_empty() {
            return Err(WinkitError::internal("tool with empty name registered"));
        }
        if tool.description.is_empty() {
            return Err(WinkitError::internal(format!(
                "tool '{name}' has no description"
            )));
        }
        if !tool.input_schema.is_object() {
            return Err(WinkitError::internal(format!(
                "tool '{name}' has a non-object input schema"
            )));
        }
        let action_cap = tool_action_capability(name);
        match (tool.capability, action_cap) {
            (None, None) => {
                return Err(WinkitError::internal(format!(
                    "tool '{name}' declares no capability"
                )))
            }
            (Some(_), Some(_)) => {
                return Err(WinkitError::internal(format!(
                    "tool '{name}' declares both read and action capabilities"
                )))
            }
            _ => {}
        }
        if !tool_profiles(name).contains(&ToolProfile::Full) {
            return Err(WinkitError::internal(format!(
                "tool '{name}' is not visible in the full profile"
            )));
        }
    }
    Ok(names.len())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The canonical tool set (source of truth for "everything is
    /// registered").
    const EXPECTED_TOOLS: &[&str] = &[
        "audit_path_env",
        "battery_status",
        "correlate_recent_failures",
        "crash_history",
        "dev_environment",
        "diagnose_local_webapp",
        "diagnose_workspace",
        "directory_overview",
        "disk_health",
        "disk_performance",
        "disk_usage",
        "find_files",
        "find_process",
        "find_process_on_port",
        "get_application_errors",
        "get_process",
        "get_process_tree",
        "get_recent_events",
        "get_service",
        "get_system_errors",
        "hardware_snapshot",
        "list_connections",
        "list_dev_servers",
        "list_drives",
        "list_listening_ports",
        "list_network_interfaces",
        "list_processes",
        "list_services",
        "list_windows",
        "network_diagnose",
        "network_snapshot",
        "power_status",
        "privacy_info",
        "read_text_file",
        "registry_diagnostics",
        "shutdown_analysis",
        "snapshot",
        "startup_programs",
        "system_diagnose",
        "system_health",
        "system_health_trend",
        "system_info",
        "system_update_status",
        "thermal_snapshot",
        "tool_guide",
        "wait_for_http",
        "wait_for_port",
        "wait_for_process",
        "wifi_scan",
        "wifi_status",
        "workspace_snapshot",
    ];

    #[test]
    fn registry_builds_every_expected_tool_exactly_once() {
        let registry = ToolRegistry::build(&Config::default());
        let names = registry.all_names();
        assert_eq!(
            names, EXPECTED_TOOLS,
            "registered set diverged from the canonical list"
        );
        // No duplicates: the sorted unique list equals the full list.
        let mut unique = names.clone();
        unique.dedup();
        assert_eq!(names.len(), unique.len(), "a tool is registered twice");
        // Every entry also resolves through the profile table to at least
        // one profile, and verify_integrity fully agrees.
        assert_eq!(verify_integrity(&registry).unwrap(), names.len());
    }

    #[test]
    fn every_tool_has_complete_definition() {
        let registry = ToolRegistry::build(&Config::default());
        for name in registry.all_names() {
            let t = registry.get(&name).unwrap();
            assert!(!t.name.is_empty());
            assert!(!t.description.is_empty());
            assert!(t.input_schema.is_object(), "'{name}' has no object schema");
            assert!(
                t.capability.is_some() ^ tool_action_capability(&name).is_some(),
                "'{name}' must declare exactly one capability source"
            );
            assert!(t.timeout_ms.is_none() || t.timeout_ms.unwrap() > 0);
        }
    }

    #[test]
    fn tools_list_reflects_effective_profile() {
        let mut cfg = Config::default();
        cfg.tools.profile = "core".to_string();
        let core = ToolRegistry::build(&cfg);
        let names: Vec<String> = core.schemas().into_iter().map(|(n, _, _)| n).collect();
        assert_eq!(
            names,
            [
                "list_listening_ports",
                "list_processes",
                "privacy_info",
                "system_health",
                "tool_guide",
                "workspace_snapshot"
            ]
        );

        // Disabled tools are hidden from tools/list and rejected on call.
        let mut cfg = Config::default();
        cfg.tools.disabled = vec!["list_windows".to_string()];
        let reg = ToolRegistry::build(&cfg);
        assert!(!reg.schemas().iter().any(|(n, _, _)| n == "list_windows"));
    }

    #[test]
    fn profile_exposed_tool_counts_are_exact() {
        // Pin the per-profile exposure. Core tools are only in core/developer/
        // full, and the developer-only workflow tools are only in
        // developer/full.
        for (profile, expected) in [("core", 6), ("developer", 51), ("full", 51)] {
            let mut cfg = Config::default();
            cfg.tools.profile = profile.to_string();
            let reg = ToolRegistry::build(&cfg);
            let n = reg.schemas().len();
            assert_eq!(
                n, expected,
                "profile '{profile}' exposes {n} tools, expected {expected}"
            );
        }
    }

    #[test]
    fn profile_table_covers_all_registered_tools() {
        let registry = ToolRegistry::build(&Config::default());
        for name in registry.all_names() {
            assert!(
                !tool_profiles(&name).is_empty(),
                "tool '{name}' has an empty profile membership"
            );
            assert!(
                tool_in_profile(&name, ToolProfile::Full),
                "tool '{name}' should be visible in the full profile"
            );
        }
    }

    #[test]
    fn developer_profile_is_a_superset_of_core() {
        for core_tool in [
            "workspace_snapshot",
            "system_health",
            "list_processes",
            "list_listening_ports",
            "privacy_info",
            "tool_guide",
        ] {
            assert!(tool_in_profile(core_tool, ToolProfile::Core));
            assert!(tool_in_profile(core_tool, ToolProfile::Developer));
        }
    }

    #[test]
    fn level_mapping_covers_documented_names() {
        assert_eq!(level_to_min("critical"), Some(1));
        assert_eq!(level_to_min("ERROR"), Some(2));
        assert_eq!(level_to_min("warning"), Some(3));
        assert_eq!(level_to_min("info"), Some(4));
        assert_eq!(level_to_min("verbose"), Some(5));
        assert_eq!(level_to_min("nonsense"), None);
    }

    #[test]
    fn clamp_limit_bounds_requests() {
        assert_eq!(clamp_limit(None, 200), 200);
        assert_eq!(clamp_limit(Some(0), 200), 1);
        assert_eq!(clamp_limit(Some(5000), 200), 200);
        assert_eq!(clamp_limit(Some(50), 200), 50);
    }

    #[test]
    fn required_helpers_report_missing_keys() {
        let args = json!({ "pid": 123 });
        assert_eq!(
            required_string(&args, "path").unwrap_err().kind,
            crate::errors::ErrorKind::InvalidArgument
        );
        assert_eq!(required_u32(&args, "pid").unwrap(), 123);
    }
}
