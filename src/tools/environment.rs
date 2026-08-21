//! Environment & OS-posture tools: startup programs, PATH audit,
//! Windows update status, and the agent-facing tool guide.
//!
//! All reads are bounded and read-only; registry access stays inside the
//! fixed allowlist (see `platform::windows::registry`).

use crate::errors::WinkitError;
use crate::permissions::Capability;
use crate::server::AppState;
use crate::tools::{clamp_limit, optional_usize, wrap, ToolDefinition};
use serde_json::{json, Value};
use std::sync::Arc;

const MAX_HOTFIXES: usize = 50;

// ---------------------------------------------------------------------------
// startup_programs
// ---------------------------------------------------------------------------

pub async fn startup_programs_handler(
    state: Arc<AppState>,
    _args: Value,
) -> Result<Value, WinkitError> {
    let programs = state.windows.startup_programs()?;
    let count = programs.len();
    let enabled = programs.iter().filter(|p| p.enabled).count();
    Ok(json!({
        "startup_programs": programs,
        "count": count,
        "enabled": enabled,
    }))
}

pub fn startup_programs_definition() -> ToolDefinition {
    ToolDefinition {
        name: "startup_programs",
        description: "List autostart entries from Run/RunOnce keys under HKLM and HKCU with their command line and enabled/disabled state (from StartupApproved). Answers \"why does X start with my PC?\" without the full registry_diagnostics payload.",
        input_schema: json!({
            "type": "object",
            "properties": {},
            "additionalProperties": false,
        }),
        capability: Some(Capability::RegistryRead),
        timeout_ms: None,
        handler: wrap(startup_programs_handler),
    }
}

// ---------------------------------------------------------------------------
// audit_path_env
// ---------------------------------------------------------------------------

pub async fn audit_path_env_handler(
    state: Arc<AppState>,
    _args: Value,
) -> Result<Value, WinkitError> {
    let audit = state.windows.path_audit()?;
    Ok(json!({
        "path_audit": audit,
    }))
}

pub fn audit_path_env_definition() -> ToolDefinition {
    ToolDefinition {
        name: "audit_path_env",
        description: "Audit the PATH environment variable: every process entry checked for existence, cross-scope duplicates, and empty ';;' entries, compared against the machine (HKLM) and user (HKCU) scope definitions. Answers \"why can't my shell find this tool?\". Read-only; %VAR% segments are expanded for existence checks only.",
        input_schema: json!({
            "type": "object",
            "properties": {},
            "additionalProperties": false,
        }),
        capability: Some(Capability::SystemRead),
        timeout_ms: None,
        handler: wrap(audit_path_env_handler),
    }
}

// ---------------------------------------------------------------------------
// system_update_status
// ---------------------------------------------------------------------------

pub async fn system_update_status_handler(
    state: Arc<AppState>,
    args: Value,
) -> Result<Value, WinkitError> {
    let max_hotfixes = clamp_limit(optional_usize(&args, "max_hotfixes"), MAX_HOTFIXES);
    let status = state.windows.update_status(max_hotfixes)?;
    Ok(json!({ "update_status": status }))
}

pub fn system_update_status_definition() -> ToolDefinition {
    ToolDefinition {
        name: "system_update_status",
        description: "Windows update posture in one call: whether a reboot is pending (Component Based Servicing / Windows Update / PendingFileRenameOperations markers), plus the most recent installed hotfixes (KB IDs, newest first). Pairs with shutdown_analysis when investigating surprise reboots.",
        input_schema: json!({
            "type": "object",
            "properties": {
                "max_hotfixes": { "type": "integer", "minimum": 1, "maximum": 50, "description": "Hotfix entries to include (default 10)." },
            },
            "additionalProperties": false,
        }),
        capability: Some(Capability::SystemRead),
        timeout_ms: None,
        handler: wrap(system_update_status_handler),
    }
}

// ---------------------------------------------------------------------------
// tool_guide — static symptom → tool routing for agents
// ---------------------------------------------------------------------------

/// The routing table. Static by design: deterministic, no I/O, always the
/// same shape, so agents can rely on it mid-task.
fn guide_entries() -> Vec<Value> {
    vec![
        json!({"symptoms": ["high memory", "RAM", "what is eating my RAM", "slow machine", "performance"], "tool": "system_health", "example_args": {}, "notes": "Grouped per-application CPU/memory with statuses; follow up with get_process or get_process_tree on a specific PID."}),
        json!({"symptoms": ["overall diagnosis", "full checkup", "something is wrong"], "tool": "system_diagnose", "example_args": {}, "notes": "Ranked evidence-backed findings across memory, storage, battery, thermal, Wi-Fi. Start here for vague complaints."}),
        json!({"symptoms": ["disk space", "disk full", "storage pressure", "free space"], "tool": "directory_overview", "example_args": {"path": "<suspect folder>"}, "notes": "Recursive size per child, largest first. Use list_drives/disk_usage to find which drive is low first."}),
        json!({"symptoms": ["read a log file", "read config", "see file contents", "tail a log"], "tool": "read_text_file", "example_args": {"path": "<absolute path>", "mode": "tail"}, "notes": "Bounded text read; head/tail/all modes; binary files are refused."}),
        json!({"symptoms": ["find a file", "locate config", "where is this file", "lost file"], "tool": "find_files", "example_args": {"root": "<start dir>", "pattern": "*.log"}, "notes": "Wildcard filename search under one root, bounded depth/results."}),
        json!({"symptoms": ["crash", "BSOD", "application error", "what crashed"], "tool": "crash_history", "example_args": {"since_minutes": 1440}, "notes": "Application errors + WER + bugchecks in one report."}),
        json!({"symptoms": ["why did my pc restart", "unexpected reboot", "power loss", "shutdown"], "tool": "shutdown_analysis", "example_args": {"since_minutes": 720}, "notes": "Classifies clean vs dirty shutdowns; pair with system_update_status to rule out update reboots."}),
        json!({"symptoms": ["pending reboot", "windows update", "hotfix", "KB"], "tool": "system_update_status", "example_args": {}, "notes": "Reboot-pending markers plus recent hotfixes."}),
        json!({"symptoms": ["port already in use", "port conflict", "who listens on port", "EADDRINUSE"], "tool": "list_listening_ports", "example_args": {}, "notes": "Then find_process_on_port(port=N) for ownership; list_connections for outbound state."}),
        json!({"symptoms": ["local server not reachable", "dev server broken", "localhost refuses connection"], "tool": "diagnose_local_webapp", "example_args": {"url": "http://localhost:3000"}, "notes": "End-to-end: port owner, HTTP probe, workspace correlation."}),
        json!({"symptoms": ["service failed", "is service running", "windows service"], "tool": "get_service", "example_args": {"name": "<service name>"}, "notes": "Use list_services first when the exact name is unknown."}),
        json!({"symptoms": ["event log", "error log", "event viewer", "recent errors"], "tool": "get_system_errors", "example_args": {"max_results": 20}, "notes": "Bound results (max_results/since_minutes); message text is truncated per event."}),
        json!({"symptoms": ["cpu temperature", "fan spinning", "overheating", "thermal"], "tool": "thermal_snapshot", "example_args": {}, "notes": "Pair with hardware_snapshot for clocks/utilization context."}),
        json!({"symptoms": ["battery health", "battery drains", "on battery"], "tool": "battery_status", "example_args": {}, "notes": "Design vs full-charge capacity, cycle count when available."}),
        json!({"symptoms": ["wifi slow", "signal strength", "wireless"], "tool": "network_diagnose", "example_args": {}, "notes": "Signal, link speed, gateway reachability; wifi_scan lists nearby networks when enabled."}),
        json!({"symptoms": ["process tree", "child processes", "which process spawned"], "tool": "get_process_tree", "example_args": {"pid": 1234}, "notes": "Bounded-depth ancestry/descendants of one PID."}),
        json!({"symptoms": ["workspace", "project scan", "repo info", "package managers"], "tool": "workspace_snapshot", "example_args": {"workspace_path": "<project dir>"}, "notes": "Languages/frameworks/scripts/git state; diagnose_workspace adds live findings."}),
        json!({"symptoms": ["dev tools missing", "node not found", "PATH broken", "command not recognized", "'git' is not recognized"], "tool": "audit_path_env", "example_args": {}, "notes": "Per-entry existence/duplicate checks; also try dev_environment to see which dev tools probe successfully."}),
        json!({"symptoms": ["startup programs", "autorun", "runs at boot", "disable startup"], "tool": "startup_programs", "example_args": {}, "notes": "Run/RunOnce entries with enabled state. WinKit is read-only: it cannot disable anything."}),
    ]
}

pub async fn tool_guide_handler(_state: Arc<AppState>, _args: Value) -> Result<Value, WinkitError> {
    Ok(json!({
        "guide": guide_entries(),
        "profiles": {
            "core": "5 safe essentials (low-latency)",
            "developer": "recommended default; all workflow + diagnostic tools",
            "full": "identical set to developer today"
        },
        "rules": [
            "Every tool is read-only: WinKit never writes, kills, or changes anything.",
            "Prefer the high-level diagnose_* tools before raw listing tools.",
            "All list tools take limits; pass them to keep responses small.",
            "Reports separate measured evidence from interpretation; findings carry stable IDs and confidence."
        ],
    }))
}

pub fn tool_guide_definition() -> ToolDefinition {
    ToolDefinition {
        name: "tool_guide",
        description: "Route a problem to the right WinKit tool: returns a symptom→tool routing table (memory, disk space, crashes, ports, logs, files, updates, PATH...) with example arguments. Call this first when unsure which tool fits the question.",
        input_schema: json!({
            "type": "object",
            "properties": {},
            "additionalProperties": false,
        }),
        capability: Some(Capability::SystemRead),
        timeout_ms: None,
        handler: wrap(tool_guide_handler),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;
    use crate::providers::mock::MockWindowsBackend;
    use crate::providers::windows::WindowsBackend;
    use crate::server::AppState;

    fn state() -> Arc<AppState> {
        let backend: Arc<dyn WindowsBackend> = Arc::new(MockWindowsBackend::with_fixtures());
        AppState::with_backend(Config::default(), backend).unwrap()
    }

    #[tokio::test]
    async fn startup_programs_uses_registry_fixture() {
        let out = startup_programs_handler(state(), json!({})).await.unwrap();
        assert_eq!(out["count"], 2);
        assert_eq!(out["enabled"], 1);
    }

    #[tokio::test]
    async fn guide_lists_entries_and_every_referenced_tool_exists() {
        let out = tool_guide_handler(state(), json!({})).await.unwrap();
        let entries = out["guide"].as_array().unwrap();
        assert!(entries.len() >= 15);
        // Every referenced tool must actually exist in the registry.
        let reg = crate::tools::ToolRegistry::build(&Config::default());
        for e in entries {
            let tool = e["tool"].as_str().unwrap();
            assert!(
                reg.get(tool).is_some(),
                "guide references unknown tool '{tool}'"
            );
            assert!(e["symptoms"].as_array().unwrap().len() >= 2);
        }
    }

    #[tokio::test]
    async fn update_status_and_path_audit_are_mocked_or_unsupported() {
        let st = state();
        // Mock backend overrides these; both must serialize cleanly.
        let status = st.windows.update_status(10).unwrap();
        assert!(status.hotfixes.len() <= 10);
        let audit = st.windows.path_audit().unwrap();
        assert_eq!(audit.total_entries, audit.process_entries.len());
    }
}
