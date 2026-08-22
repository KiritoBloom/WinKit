//! Tool-level tests against the mock Windows backend, through the full
//! dispatch path (`server::registry::call_tool`). Run with
//! `cargo test --features mocks`. Nothing touches the real machine.

use serde_json::{json, Value};
use std::sync::Arc;
use winkit::config::Config;
use winkit::errors::ErrorKind;
use winkit::providers::mock::MockWindowsBackend;
use winkit::providers::windows::WindowsBackend;
use winkit::server::{registry, AppState};

/// State with only the windows provider enabled, backed by the mock.
fn mock_state(permission_mode: &str) -> Arc<AppState> {
    let mut config = Config::default();
    config.permissions.mode = permission_mode.to_string();
    config.providers.enabled = vec!["windows".to_string()];
    let backend: Arc<dyn WindowsBackend> = Arc::new(MockWindowsBackend::with_fixtures());
    AppState::with_backend(config, backend).unwrap()
}

async fn call(
    state: &Arc<AppState>,
    name: &str,
    args: Value,
) -> Result<Value, winkit::errors::WinkitError> {
    registry::call_tool(state, name, args).await
}

#[tokio::test]
async fn system_info_returns_mock_os_data() {
    let state = mock_state("read_only");
    let out = call(&state, "system_info", json!({})).await.unwrap();
    assert_eq!(out["system"]["hostname"], "mock-host");
    assert_eq!(out["system"]["cpu_cores"], 8);
    assert_eq!(out["system"]["logical_processors"], 16);
    assert!(out["permissions"]["mode"] == "read_only");
    let provider_ids: Vec<&str> = out["providers"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|p| p["id"].as_str())
        .collect();
    assert!(provider_ids.contains(&"windows"));
    assert!(!provider_ids.contains(&"chrome"));
}

#[tokio::test]
async fn list_processes_respects_limit() {
    let state = mock_state("read_only");
    let out = call(&state, "list_processes", json!({ "limit": 2 }))
        .await
        .unwrap();
    assert_eq!(out["count"], 2);
    assert!(out["truncated"].as_bool().unwrap());
}

#[tokio::test]
async fn find_process_on_port_uses_mock_data() {
    let state = mock_state("read_only");
    let out = call(&state, "find_process_on_port", json!({ "port": 3000 }))
        .await
        .unwrap();
    assert_eq!(out["port"]["process_name"], "node.exe");
    let missing = call(&state, "find_process_on_port", json!({ "port": 9999 }))
        .await
        .unwrap();
    // The requested port is preserved in the no-listener response so callers
    // can correlate the message with their query.
    assert_eq!(missing["port"], 9999);
    assert!(missing["message"].as_str().unwrap().contains("9999"));
}

#[tokio::test]
async fn startup_programs_reports_status_impact_and_hidden_entries() {
    let state = mock_state("read_only");
    let out = call(&state, "startup_programs", json!({})).await.unwrap();
    // Fixture: OneDrive (enabled run), OldTool (disabled run),
    // TelemetrySetup (hidden run_once).
    assert_eq!(out["count"], 3);
    assert_eq!(out["enabled"], 2);
    assert_eq!(out["disabled"], 1);
    assert_eq!(out["hidden_count"], 1);
    let summary = out["impact_summary"].as_object().unwrap();
    let total_by_impact: usize = summary.values().map(|v| v.as_u64().unwrap() as usize).sum();
    assert_eq!(total_by_impact, 3, "impact summary covers every entry");
    let programs = out["startup_programs"].as_array().unwrap();
    for entry in programs {
        // Every entry carries the enrichment fields.
        assert!(entry["entry_type"].is_string());
        assert!(entry["hidden"].is_boolean());
        assert!(
            ["high", "medium", "low", "none"]
                .contains(&entry["impact"].as_str().unwrap_or_default()),
            "unexpected impact level: {entry}"
        );
        assert!(entry["impact_reasons"].is_array());
    }
    // Disabled entries never report a measurable impact level.
    assert!(programs
        .iter()
        .all(|e| e["enabled"] == false || e["impact"] != "none"));
    // The boot-timing limitation note is always included.
    let notes = out["notes"].as_array().unwrap();
    assert!(notes
        .iter()
        .any(|n| n.as_str().unwrap_or_default().contains("boot-phase timing")));
}

#[tokio::test]
async fn get_process_unknown_pid_is_invalid_argument() {
    let state = mock_state("read_only");
    let err = call(&state, "get_process", json!({ "pid": 999999 }))
        .await
        .unwrap_err();
    assert_eq!(err.kind, ErrorKind::InvalidArgument);
}

#[tokio::test]
async fn events_level_validation_rejects_bad_levels() {
    let state = mock_state("read_only");
    let err = call(&state, "get_recent_events", json!({ "level": "bogus" }))
        .await
        .unwrap_err();
    assert_eq!(err.kind, ErrorKind::InvalidArgument);
    // Rejected up front by schema validation: names the allowed values.
    assert!(err.message.contains("must be one of"));
    assert!(err.message.contains("verbose"));
}

#[tokio::test]
async fn get_application_errors_defaults_to_error_level() {
    let state = mock_state("read_only");
    let out = call(&state, "get_application_errors", json!({}))
        .await
        .unwrap();
    assert_eq!(out["log"], "Application");
    assert!(!out["events"].as_array().unwrap().is_empty());
}

#[tokio::test]
async fn missing_required_argument_is_invalid_argument() {
    let state = mock_state("read_only");
    let err = call(&state, "get_service", json!({})).await.unwrap_err();
    assert_eq!(err.kind, ErrorKind::InvalidArgument);
    assert!(err.message.contains("name"));
}

#[tokio::test]
async fn safe_mode_denies_hardware_tools() {
    let state = mock_state("safe");
    let err = call(&state, "hardware_snapshot", json!({}))
        .await
        .unwrap_err();
    assert_eq!(err.kind, ErrorKind::PermissionDenied);
    assert!(err.message.contains("hardware.read"));
}

#[tokio::test]
async fn unknown_tool_is_invalid_argument() {
    let state = mock_state("read_only");
    let err = call(&state, "no_such_tool", json!({})).await.unwrap_err();
    assert_eq!(err.kind, ErrorKind::InvalidArgument);
    assert!(err.message.contains("unknown tool"));
}

#[tokio::test]
async fn disabled_tool_is_rejected() {
    let mut config = Config::default();
    config.permissions.mode = "read_only".to_string();
    config.providers.enabled = vec!["windows".to_string()];
    config.tools.disabled = vec!["list_windows".to_string()];
    let backend: Arc<dyn WindowsBackend> = Arc::new(MockWindowsBackend::with_fixtures());
    let state = AppState::with_backend(config, backend).unwrap();
    let err = call(&state, "list_windows", json!({})).await.unwrap_err();
    assert_eq!(err.kind, ErrorKind::InvalidArgument);
    assert!(err.message.contains("disabled"));
}

#[tokio::test]
async fn snapshot_is_structured_and_bounded() {
    let state = mock_state("read_only");
    let out = call(&state, "snapshot", json!({})).await.unwrap();
    assert!(out["system"]["os_name"].is_string());
    assert!(out["resources"]["cpu_busy_percent"].is_number());
    assert!(out["processes"]["count"].is_number());
    assert!(out["storage"].is_array());
    assert!(out["network"]["listening_port_count"].is_number());
    assert!(out["services"]["running"].is_number());
    assert!(out["windows"]["count"].is_number());
}
