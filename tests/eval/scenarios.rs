//! The failure evaluation suite (see `tests/eval/README.md`).
//!
//! Every scenario is deterministic and fixture-backed: the Windows layer is
//! a mock backend, HTTP scenarios use loopback-only test servers, and
//! workspace scenarios use temporary directories.
//!
//! Each scenario asserts the report status, important evidence and finding
//! ids, supporting/contradicting evidence where applicable, redaction,
//! bounded output, permission behavior, and the absence of root-cause
//! claims. Run with `cargo test --features mocks --test eval`.

use crate::helpers::{
    assert_bounded_envelope, assert_evidence_ids_resolve, assert_evidence_subject,
    assert_no_root_cause_claims, call, default_config, eval_state, finding, http_response,
    Behavior, ScenarioBackend, TestServer, WorkspaceFixture, PORT_GUARD,
};
use serde_json::json;
use std::sync::Arc;
use winkit::config::Config;
use winkit::errors::ErrorKind;
use winkit::models::{DriveInfo, ProcessInfo};
use winkit::providers::mock::MockWindowsBackend;
use winkit::server::AppState;

// ---------------------------------------------------------------------------
// Fixture builders
// ---------------------------------------------------------------------------

/// A light process table (no heavy chrome group) with no listeners.
fn quiet_processes() -> Vec<ProcessInfo> {
    let p = |pid: u32, name: &str, ws: u64| ProcessInfo {
        pid,
        name: name.to_string(),
        parent_pid: Some(4),
        executable_path: Some(format!("C:\\Program Files\\{name}")),
        command_line: None,
        working_set_bytes: Some(ws),
        private_bytes: Some(ws / 2),
        threads: Some(8),
        start_time: Some("2026-08-13T08:00:00.000Z".to_string()),
        cpu_time_ms: Some(10_000),
        cpu_percent: None,
    };
    vec![
        p(4, "System", 0),
        p(771, "svchost.exe", 80_000_000),
        p(900, "node.exe", 120_000_000),
        p(1010, "explorer.exe", 250_000_000),
    ]
}

fn quiet_backend() -> ScenarioBackend {
    let backend = MockWindowsBackend {
        processes: quiet_processes(),
        ports: Vec::new(),
        events: Vec::new(),
        drives: vec![DriveInfo {
            root: "C:\\".into(),
            kind: "fixed".into(),
            total_bytes: Some(1_000_000_000_000),
            free_bytes: Some(400_000_000_000),
            used_bytes: Some(600_000_000_000),
            percent_used: Some(60.0),
        }],
        ..Default::default()
    };
    ScenarioBackend {
        inner: backend,
        snapshot: Some(winkit::models::ResourceSnapshot {
            cpu_busy_percent: Some(8.0),
            cpu_busy_percent_basis: "system_capacity_all_cores".into(),
            memory_load_percent: Some(40.0),
            total_memory_bytes: Some(16_000_000_000),
            available_memory_bytes: Some(9_600_000_000),
        }),
        ..ScenarioBackend::default()
    }
}

/// The default fixture mock with a custom system snapshot.
fn pressure_backend(cpu: f64, mem_load: f64, available: u64) -> ScenarioBackend {
    ScenarioBackend::with_snapshot(cpu, mem_load, 16_000_000_000, available)
}

/// The default fixture mock with a low-disk drive list covering `root`'s
/// drive (so the scenario is deterministic on any machine).
fn low_disk_backend(root: &str) -> ScenarioBackend {
    // `root` is a canonicalized path that may carry the `\\?\` extended-
    // length prefix; strip it before splitting off the drive letter.
    let drive = root
        .trim_start_matches("\\\\?\\")
        .split('\\')
        .next()
        .map(|d| format!("{d}\\"))
        .unwrap_or_else(|| "C:\\".to_string());
    let backend = MockWindowsBackend {
        drives: vec![DriveInfo {
            root: drive.clone(),
            kind: "fixed".into(),
            total_bytes: Some(1_000_000_000_000),
            free_bytes: Some(4_000_000_000),
            used_bytes: Some(996_000_000_000),
            percent_used: Some(99.6),
        }],
        ..Default::default()
    };
    ScenarioBackend {
        inner: backend,
        snapshot: Some(winkit::models::ResourceSnapshot {
            cpu_busy_percent: Some(20.0),
            cpu_busy_percent_basis: "system_capacity_all_cores".into(),
            memory_load_percent: Some(50.0),
            total_memory_bytes: Some(16_000_000_000),
            available_memory_bytes: Some(8_000_000_000),
        }),
        ..ScenarioBackend::default()
    }
}

/// A chrome-heavy process table: 3.5 GB working set, 42.5% CPU (the mock
/// reports the aggregate CPU for chrome.exe groups).
fn heavy_chrome_backend() -> ScenarioBackend {
    let processes = vec![
        ProcessInfo {
            pid: 420,
            name: "chrome.exe".to_string(),
            parent_pid: Some(4),
            executable_path: Some(
                "C:\\Program Files\\Google\\Chrome\\Application\\chrome.exe".to_string(),
            ),
            command_line: None,
            working_set_bytes: Some(1_900_000_000),
            private_bytes: Some(900_000_000),
            threads: Some(40),
            start_time: Some("2026-08-13T08:00:00.000Z".to_string()),
            cpu_time_ms: Some(200_000),
            cpu_percent: None,
        },
        ProcessInfo {
            pid: 521,
            name: "chrome.exe".to_string(),
            parent_pid: Some(420),
            executable_path: Some(
                "C:\\Program Files\\Google\\Chrome\\Application\\chrome.exe".to_string(),
            ),
            command_line: None,
            working_set_bytes: Some(1_100_000_000),
            private_bytes: Some(500_000_000),
            threads: Some(16),
            start_time: Some("2026-08-13T08:00:00.000Z".to_string()),
            cpu_time_ms: Some(150_000),
            cpu_percent: None,
        },
        ProcessInfo {
            pid: 618,
            name: "chrome.exe".to_string(),
            parent_pid: Some(420),
            executable_path: Some(
                "C:\\Program Files\\Google\\Chrome\\Application\\chrome.exe".to_string(),
            ),
            command_line: None,
            working_set_bytes: Some(500_000_000),
            private_bytes: Some(250_000_000),
            threads: Some(10),
            start_time: Some("2026-08-13T08:00:00.000Z".to_string()),
            cpu_time_ms: Some(80_000),
            cpu_percent: None,
        },
        ProcessInfo {
            pid: 1010,
            name: "explorer.exe".to_string(),
            parent_pid: None,
            executable_path: Some("C:\\Windows\\explorer.exe".to_string()),
            command_line: None,
            working_set_bytes: Some(250_000_000),
            private_bytes: Some(100_000_000),
            threads: Some(30),
            start_time: Some("2026-08-13T08:00:00.000Z".to_string()),
            cpu_time_ms: Some(30_000),
            cpu_percent: None,
        },
    ];
    let backend = MockWindowsBackend {
        processes,
        ..Default::default()
    };
    ScenarioBackend {
        inner: backend,
        snapshot: Some(winkit::models::ResourceSnapshot {
            cpu_busy_percent: Some(50.0),
            cpu_busy_percent_basis: "system_capacity_all_cores".into(),
            memory_load_percent: Some(45.0),
            total_memory_bytes: Some(16_000_000_000),
            available_memory_bytes: Some(8_800_000_000),
        }),
        ..ScenarioBackend::default()
    }
}

// ---------------------------------------------------------------------------
// Scenario 1 — healthy / quiet machine
// ---------------------------------------------------------------------------

#[tokio::test]
async fn scenario_01_healthy_quiet_machine() {
    let state = eval_state(default_config(), quiet_backend());

    let health = call(&state, "system_health", json!({})).await.unwrap();
    assert_eq!(health["system"]["memory_pressure"], false);
    assert_eq!(health["system"]["drives"][0]["low_disk_space"], false);
    assert_eq!(health["issues"].as_array().unwrap().len(), 0);
    for app in health["applications"].as_array().unwrap() {
        assert_eq!(app["status"], "normal", "quiet apps stay normal");
    }
    let serialized = serde_json::to_string(&health).unwrap();
    assert!(serialized.len() <= 256 * 1024);
    assert_no_root_cause_claims(&health);

    // system_diagnose agrees with system_health (shared classification).
    let diagnose = call(&state, "system_diagnose", json!({})).await.unwrap();
    assert_eq!(
        diagnose["diagnosis"]["report"]["status"],
        "no_supported_signal_detected"
    );
    assert_eq!(
        diagnose["diagnosis"]["findings"].as_array().unwrap().len(),
        0
    );
    let clean = diagnose["diagnosis"]["checked_clean"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|v| v.as_str())
        .collect::<Vec<_>>();
    assert!(clean.contains(&"system memory pressure"));
    assert!(clean.contains(&"storage pressure"));
    assert_no_root_cause_claims(&diagnose);
}

// ---------------------------------------------------------------------------
// Scenario 2 — high memory pressure
// ---------------------------------------------------------------------------

#[tokio::test]
async fn scenario_02_high_memory_pressure() {
    let state = eval_state(default_config(), pressure_backend(30.0, 94.0, 900_000_000));

    let health = call(&state, "system_health", json!({})).await.unwrap();
    assert_eq!(health["system"]["memory_pressure"], true);
    assert_eq!(health["system"]["memory_load_percent"], 94.0);
    let pressure_issue = health["issues"]
        .as_array()
        .unwrap()
        .iter()
        .find(|i| i["kind"] == "memory_pressure")
        .expect("memory_pressure issue present");
    assert_eq!(pressure_issue["category"], "memory_pressure");
    assert!(pressure_issue["score"].as_u64().unwrap() > 0);
    assert_no_root_cause_claims(&health);

    let diagnose = call(&state, "system_diagnose", json!({})).await.unwrap();
    let findings = diagnose["diagnosis"]["findings"].as_array().unwrap();
    assert!(
        findings.iter().any(|f| f["category"] == "memory_pressure"),
        "system_diagnose ranks a memory-pressure finding"
    );
    assert_eq!(
        diagnose["diagnosis"]["report"]["status"],
        "signals_detected"
    );
    assert_no_root_cause_claims(&diagnose);

    // diagnose_workspace correlates the pressure into its envelope.
    let ws = WorkspaceFixture::node_workspace();
    let out = call(
        &state,
        "diagnose_workspace",
        json!({ "workspace_path": ws.canonical(), "dev_server_ports": [3000] }),
    )
    .await
    .unwrap();
    assert_eq!(out["status"], "issues_detected");
    let mem = finding(&out, "memory-pressure");
    assert_eq!(mem["confidence"], "observed");
    assert_eq!(mem["category"], "system");
    assert!(!mem["supporting_evidence"].as_array().unwrap().is_empty());
    assert_evidence_ids_resolve(&out);
    assert_bounded_envelope(&out);
    assert_no_root_cause_claims(&out);
}

// ---------------------------------------------------------------------------
// Scenario 3 — low disk space
// ---------------------------------------------------------------------------

#[tokio::test]
async fn scenario_03_low_disk_space() {
    let ws = WorkspaceFixture::node_workspace();
    let state = eval_state(default_config(), low_disk_backend(&ws.canonical()));

    let health = call(&state, "system_health", json!({})).await.unwrap();
    let disk_issue = health["issues"]
        .as_array()
        .unwrap()
        .iter()
        .find(|i| i["kind"] == "low_disk_space")
        .expect("low_disk_space issue present");
    assert_eq!(disk_issue["category"], "storage");
    assert!(disk_issue["value"].as_str().unwrap().contains("GB free"));
    assert_no_root_cause_claims(&health);

    let diagnose = call(&state, "system_diagnose", json!({})).await.unwrap();
    let findings = diagnose["diagnosis"]["findings"].as_array().unwrap();
    assert_eq!(
        findings[0]["category"], "storage",
        "storage pressure (1% free) ranks first"
    );
    assert!(findings[0]["score"].as_u64().unwrap() >= 90);
    assert!(
        !diagnose["diagnosis"]["checked_clean"]
            .as_array()
            .unwrap()
            .iter()
            .any(|v| v == "storage pressure"),
        "a measured, failing dimension is never listed as checked clean"
    );

    // diagnose_workspace on the low-disk drive surfaces the finding.
    let out = call(
        &state,
        "diagnose_workspace",
        json!({ "workspace_path": ws.canonical(), "dev_server_ports": [3000] }),
    )
    .await
    .unwrap();
    let low = finding(&out, "low-disk-space");
    assert_eq!(low["category"], "system");
    assert_evidence_ids_resolve(&out);
    assert_bounded_envelope(&out);
    assert_no_root_cause_claims(&out);
}

// ---------------------------------------------------------------------------
// Scenario 4 — heavy application process
// ---------------------------------------------------------------------------

#[tokio::test]
async fn scenario_04_heavy_application_process() {
    let state = eval_state(default_config(), heavy_chrome_backend());

    let health = call(&state, "system_health", json!({})).await.unwrap();
    let chrome = health["applications"]
        .as_array()
        .unwrap()
        .iter()
        .find(|a| a["name"] == "chrome")
        .expect("chrome application group present");
    assert_eq!(chrome["status"], "high_cpu_and_memory");
    assert_eq!(chrome["cpu_percent_basis"], "system_capacity_all_cores");
    assert!(chrome["total_working_set_bytes"].as_u64().unwrap() > 2_000_000_000);
    let kinds: Vec<&str> = health["issues"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|i| i["kind"].as_str())
        .collect();
    assert!(
        kinds.contains(&"high_cpu"),
        "high_cpu issue present: {kinds:?}"
    );
    assert!(
        kinds.contains(&"high_memory"),
        "high_memory issue present: {kinds:?}"
    );
    assert_no_root_cause_claims(&health);

    // The documented limitation: per-process CPU percent is null; the
    // aggregate basis is explicit instead.
    let procs = call(&state, "list_processes", json!({ "limit": 5 }))
        .await
        .unwrap();
    for p in procs["processes"].as_array().unwrap() {
        if p["name"] == "chrome.exe" {
            assert!(
                p["cpu_percent"].is_null(),
                "per-process cpu_percent stays null"
            );
        }
    }
}

// ---------------------------------------------------------------------------
// Scenario 5 — workspace metadata discovery
// ---------------------------------------------------------------------------

#[tokio::test]
async fn scenario_05_workspace_metadata_discovery() {
    let ws = WorkspaceFixture::node_workspace();
    let state = eval_state(default_config(), quiet_backend());

    let out = call(
        &state,
        "workspace_snapshot",
        json!({ "workspace_path": ws.canonical(), "detail": "detailed" }),
    )
    .await
    .unwrap();
    assert_eq!(out["root_is_valid"], true);
    assert_eq!(out["truncated"], false);
    let languages: Vec<&str> = out["languages"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|v| v.as_str())
        .collect();
    assert!(languages.contains(&"javascript"));
    assert!(languages.contains(&"typescript"));
    assert!(out["package_managers"]
        .as_array()
        .unwrap()
        .iter()
        .any(|v| v == "npm"));
    assert!(out["scripts"]
        .as_array()
        .unwrap()
        .iter()
        .any(|v| v == "dev"));
    assert_eq!(out["projects"], 1);

    // .env is never opened; the fixture's secret value must not appear.
    let excluded: Vec<&str> = out["excluded_secret_files"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|v| v.as_str())
        .collect();
    assert!(
        excluded.iter().any(|e| e.contains(".env")),
        "excluded: {excluded:?}"
    );
    let serialized = serde_json::to_string(&out).unwrap();
    assert!(!serialized.contains("EVAL_SUPER_SECRET_TOKEN_9f2c"));
    assert!(!serialized.contains("do-not-leak-this-value"));

    // include_environment is a documented no-op that never reads env blocks.
    let env = call(
        &state,
        "workspace_snapshot",
        json!({ "workspace_path": ws.canonical(), "include_environment": true }),
    )
    .await
    .unwrap();
    assert!(env["data_excluded"]
        .as_array()
        .unwrap()
        .iter()
        .any(|v| v == "raw_environment_blocks_are_never_read"));
    assert_no_root_cause_claims(&out);
}

// ---------------------------------------------------------------------------
// Scenario 6 — nested project detection
// ---------------------------------------------------------------------------

#[tokio::test]
async fn scenario_06_nested_project_detection() {
    let ws = WorkspaceFixture::monorepo_workspace();
    let state = eval_state(default_config(), quiet_backend());

    let out = call(
        &state,
        "workspace_snapshot",
        json!({ "workspace_path": ws.canonical(), "detail": "detailed" }),
    )
    .await
    .unwrap();
    assert_eq!(out["root_is_valid"], true);
    assert!(
        out["projects"].as_u64().unwrap() >= 2,
        "nested project detected"
    );
    let manifests: Vec<String> = out["manifests"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| m["path"].as_str().unwrap_or("").to_string())
        .collect();
    assert!(manifests.iter().any(|m| m == "package.json"));
    assert!(
        manifests.iter().any(|m| m == "packages/lib/package.json"),
        "nested manifest discovered: {manifests:?}"
    );
    let languages: Vec<&str> = out["languages"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|v| v.as_str())
        .collect();
    assert!(languages.contains(&"typescript"));
    assert_no_root_cause_claims(&out);
}

// ---------------------------------------------------------------------------
// Scenario 7 — dev server discovery (related to the workspace)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn scenario_07_dev_server_discovery() {
    let ws = WorkspaceFixture::node_workspace();
    // Default fixtures: node.exe listens on port 3000; a node workspace
    // matches the stack, so the listener is related.
    let state = eval_state(default_config(), ScenarioBackend::with_fixtures());

    let servers = call(
        &state,
        "list_dev_servers",
        json!({ "workspace_path": ws.canonical(), "ports": [3000], "detail": "normal" }),
    )
    .await
    .unwrap();
    let entry = &servers["listeners"][0];
    assert_eq!(entry["port"], 3000);
    assert_eq!(entry["pid"], 900);
    assert_eq!(entry["process_name"], "node.exe");
    assert_eq!(entry["related_to_workspace"], true);

    let out = call(
        &state,
        "diagnose_workspace",
        json!({ "workspace_path": ws.canonical(), "dev_server_ports": [3000] }),
    )
    .await
    .unwrap();
    assert_eq!(out["status"], "ok");
    assert_eq!(out["findings"].as_array().unwrap().len(), 0);
    assert!(out["checked"].as_array().unwrap().iter().any(|c| c
        .as_str()
        .unwrap_or("")
        .contains("port 3000 owned by a workspace-related process")));
    assert_bounded_envelope(&out);
}

// ---------------------------------------------------------------------------
// Scenario 8 — port owned by an unrelated process
// ---------------------------------------------------------------------------

#[tokio::test]
async fn scenario_08_port_owned_by_unrelated_process() {
    // A Rust workspace: node.exe on port 3000 does not match the stack.
    let ws = WorkspaceFixture::rust_workspace();
    let state = eval_state(default_config(), ScenarioBackend::with_fixtures());

    let out = call(
        &state,
        "diagnose_workspace",
        json!({ "workspace_path": ws.canonical(), "dev_server_ports": [3000], "include_events": true }),
    )
    .await
    .unwrap();
    assert_eq!(out["status"], "issues_detected");

    let unrelated = finding(&out, "port-unrelated-process");
    assert_eq!(unrelated["severity"], "high");
    assert_eq!(unrelated["confidence"], "confirmed");
    assert_eq!(unrelated["category"], "port");
    assert!(!unrelated["supporting_evidence"]
        .as_array()
        .unwrap()
        .is_empty());
    assert_eq!(
        unrelated["contradicting_evidence"]
            .as_array()
            .unwrap()
            .len(),
        0,
        "no evidence contradicts the missing relationship"
    );
    // Projected findings carry evidence ids; the aggregated next-tools list
    // lives on the envelope itself.
    assert!(out["recommended_next_tools"]
        .as_array()
        .unwrap()
        .iter()
        .any(|t| t == "list_dev_servers"));
    assert_evidence_ids_resolve(&out);

    // The mock event log carries one error → a low-severity correlation
    // finding that explicitly does not claim causality.
    let events = finding(&out, "recent-error-events");
    assert_eq!(events["severity"], "low");
    assert_eq!(events["confidence"], "observed");
    assert_evidence_subject(&out, "Recent error events");
    assert_bounded_envelope(&out);
    assert_no_root_cause_claims(&out);

    // correlate_recent_failures agrees and stays hypothesis-only.
    let corr = call(
        &state,
        "correlate_recent_failures",
        json!({ "port": 3000, "workspace_path": ws.canonical() }),
    )
    .await
    .unwrap();
    let cluster = finding(&corr, "failure-cluster");
    assert_eq!(cluster["confidence"], "likely");
    assert!(!cluster["supporting_evidence"]
        .as_array()
        .unwrap()
        .is_empty());
    // The heuristic-co-occurrence disclaimer travels with the correlation
    // evidence, so agents see it next to what it qualifies.
    assert!(corr["evidence"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|e| e["subject"].as_str().unwrap_or("") == "Signal correlation")
        .any(|e| e["limitation"]
            .as_str()
            .unwrap_or("")
            .contains("not proven causality")));
    assert_no_root_cause_claims(&corr);
}

// ---------------------------------------------------------------------------
// Scenario 9 — connection refused
// ---------------------------------------------------------------------------

#[tokio::test]
async fn scenario_09_connection_refused() {
    let _guard = PORT_GUARD.lock().await;
    let port = TestServer::free_port();
    let state = eval_state(default_config(), ScenarioBackend::with_fixtures());

    let out = call(
        &state,
        "diagnose_local_webapp",
        json!({ "url": format!("http://127.0.0.1:{port}/") }),
    )
    .await
    .unwrap();
    assert_eq!(out["status"], "issues_detected");
    let refused = finding(&out, "connection-refused");
    assert_eq!(refused["severity"], "high");
    assert_eq!(refused["category"], "server");
    assert_eq!(refused["confidence"], "confirmed");
    assert!(!refused["supporting_evidence"]
        .as_array()
        .unwrap()
        .is_empty());
    let outcome = out["evidence"]
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["source"] == "http_probe")
        .and_then(|e| e["value"]["outcome"].as_str())
        .unwrap_or("");
    assert!(
        outcome == "connection_refused" || outcome == "connection_timeout",
        "unexpected probe outcome: {outcome}"
    );
    assert!(out["port_owner_related_to_workspace"].is_null());
    assert_evidence_ids_resolve(&out);
    assert_bounded_envelope(&out);
    assert_no_root_cause_claims(&out);
}

// ---------------------------------------------------------------------------
// Scenario 10 — HTTP 4xx
// ---------------------------------------------------------------------------

#[tokio::test]
async fn scenario_10_http_4xx() {
    let server = TestServer::new(Behavior::Respond {
        responses: vec![http_response(404, "not found")],
        delay_ms: 0,
    });
    let state = eval_state(default_config(), ScenarioBackend::with_fixtures());

    let out = call(
        &state,
        "diagnose_local_webapp",
        json!({ "url": server.url() }),
    )
    .await
    .unwrap();
    assert_eq!(out["status"], "issues_detected");
    let four = finding(&out, "http-4xx");
    assert_eq!(four["severity"], "medium");
    assert_eq!(four["category"], "server");
    assert!(four["explanation"].as_str().unwrap().contains("HTTP 404"));
    let evidence = out["evidence"].as_array().unwrap();
    let probe = evidence
        .iter()
        .find(|e| e["source"] == "http_probe")
        .expect("http_probe evidence present");
    assert_eq!(probe["value"]["status"], 404);
    assert_eq!(probe["value"]["outcome"], "http_error");
    assert_eq!(probe["value"]["body_bytes"], 0, "bodies are never returned");
    assert_evidence_ids_resolve(&out);
    assert_bounded_envelope(&out);
    assert_no_root_cause_claims(&out);
}

// ---------------------------------------------------------------------------
// Scenario 11 — HTTP 5xx
// ---------------------------------------------------------------------------

#[tokio::test]
async fn scenario_11_http_5xx() {
    let server = TestServer::new(Behavior::Respond {
        responses: vec![http_response(500, "boom")],
        delay_ms: 0,
    });
    let state = eval_state(default_config(), ScenarioBackend::with_fixtures());

    let out = call(
        &state,
        "diagnose_local_webapp",
        json!({ "url": server.url() }),
    )
    .await
    .unwrap();
    assert_eq!(out["status"], "issues_detected");
    let five = finding(&out, "http-5xx");
    assert_eq!(five["severity"], "high");
    assert_eq!(five["category"], "server");
    let probe = out["evidence"]
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["source"] == "http_probe")
        .expect("http_probe evidence present");
    assert_eq!(probe["value"]["status"], 500);
    assert_eq!(probe["value"]["outcome"], "http_error");
    assert_evidence_ids_resolve(&out);
    assert_bounded_envelope(&out);
    assert_no_root_cause_claims(&out);
}

// ---------------------------------------------------------------------------
// Scenario 12 — slow / timing-out HTTP server
// ---------------------------------------------------------------------------

#[tokio::test]
async fn scenario_12_slow_or_timing_out_server() {
    // 12a: a slow-but-reachable server triggers the slow-response finding.
    let server = TestServer::new(Behavior::Respond {
        responses: vec![http_response(200, "finally")],
        delay_ms: 2_100,
    });
    let state = eval_state(default_config(), ScenarioBackend::with_fixtures());
    let out = call(
        &state,
        "diagnose_local_webapp",
        json!({ "url": server.url() }),
    )
    .await
    .unwrap();
    assert_eq!(out["status"], "issues_detected");
    let slow = finding(&out, "slow-response");
    assert_eq!(slow["severity"], "low");
    assert_eq!(slow["confidence"], "observed");
    let probe = out["evidence"]
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["source"] == "http_probe")
        .expect("http_probe evidence present");
    assert!(probe["value"]["elapsed_ms"].as_u64().unwrap() >= 2_000);
    assert_eq!(probe["value"]["status"], 200);
    assert_evidence_ids_resolve(&out);
    assert_no_root_cause_claims(&out);

    // 12b: a server that never answers is bounded by the probe deadline and
    // reported as a connection failure, never a hang.
    let hanging = TestServer::new(Behavior::Hold);
    let mut config = default_config();
    config.web.max_http_ms = 800;
    let state = eval_state(config, ScenarioBackend::with_fixtures());
    let out = call(
        &state,
        "diagnose_local_webapp",
        json!({ "url": hanging.url() }),
    )
    .await
    .unwrap();
    assert_eq!(out["status"], "issues_detected");
    assert_evidence_ids_resolve(&out);
    let probe = out["evidence"]
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["source"] == "http_probe")
        .expect("http_probe evidence present");
    assert_eq!(probe["value"]["outcome"], "connection_timeout");
    assert!(
        probe["value"]["elapsed_ms"].as_u64().unwrap() < 3_000,
        "probe is bounded by max_http_ms, got {} ms",
        probe["value"]["elapsed_ms"]
    );
    let refused = finding(&out, "connection-refused");
    assert_eq!(refused["severity"], "high");
    assert_bounded_envelope(&out);
    assert_no_root_cause_claims(&out);
}

// ---------------------------------------------------------------------------
// Scenario 14 — redaction / privacy boundary
// ---------------------------------------------------------------------------

#[tokio::test]
async fn scenario_16_redaction_and_privacy_boundary() {
    // 16a: .env secrets never leak from workspace_snapshot.
    let ws = WorkspaceFixture::node_workspace();
    let state = eval_state(default_config(), quiet_backend());
    let out = call(
        &state,
        "workspace_snapshot",
        json!({ "workspace_path": ws.canonical(), "detail": "detailed" }),
    )
    .await
    .unwrap();
    let serialized = serde_json::to_string(&out).unwrap();
    assert!(!serialized.contains("EVAL_SUPER_SECRET_TOKEN_9f2c"));
    assert!(!serialized.contains("do-not-leak-this-value"));

    // 16b: privacy_info is explicit about the posture.
    let info = call(&state, "privacy_info", json!({})).await.unwrap();
    assert_eq!(info["permission"]["mode"], "read_only");
    assert_eq!(info["tool_profile"], "developer");
    assert_eq!(info["telemetry"]["enabled"], false);
    assert_eq!(info["external_url_policy"]["allow_external"], false);
    assert_eq!(info["history_policy"], "no_history_persisted");
    let excluded: Vec<&str> = info["excluded_data"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|v| v.as_str())
        .collect();
    for category in [
        "cookies",
        "authorization headers",
        "request bodies",
        "credentials",
        "tokens",
    ] {
        assert!(
            excluded.contains(&category),
            "excluded data lists {category}"
        );
    }
    assert_no_root_cause_claims(&info);

    // 16c: external URLs are blocked by default with a stable report.
    let out = call(
        &state,
        "diagnose_local_webapp",
        json!({ "url": "http://example.com/" }),
    )
    .await
    .unwrap();
    assert_eq!(out["status"], "blocked");
    let rejected = finding(&out, "url-rejected");
    assert_eq!(rejected["severity"], "high");
    assert_eq!(rejected["category"], "server");
    assert_no_root_cause_claims(&out);

    // 16d: a caller-supplied URL carrying userinfo is rejected AND never
    // echoed with the credentials in the report.
    let server = TestServer::new(Behavior::Respond {
        responses: vec![http_response(200, "ok")],
        delay_ms: 0,
    });
    let out = call(
        &state,
        "diagnose_local_webapp",
        json!({ "url": format!("http://alice:EVAL_SECRET_PW@127.0.0.1:{}/", server.port) }),
    )
    .await
    .unwrap();
    let serialized = serde_json::to_string(&out).unwrap();
    assert!(
        !serialized.contains("EVAL_SECRET_PW"),
        "credentials are never echoed into reports"
    );

    // 16e: safe mode denies hardware reads (permission boundary).
    let mut config = default_config();
    config.permissions.mode = "safe".to_string();
    let safe_state = eval_state(config, quiet_backend());
    let err = call(&safe_state, "hardware_snapshot", json!({}))
        .await
        .unwrap_err();
    assert_eq!(err.kind, ErrorKind::PermissionDenied);
    assert!(err.message.contains("hardware.read"));

    // 16f: the report envelope for a healthy web app carries no secrets.
    let server = TestServer::new(Behavior::Respond {
        responses: vec![http_response(200, "EVAL_SECRET_BODY_7f3a")],
        delay_ms: 0,
    });
    let out = call(
        &state,
        "diagnose_local_webapp",
        json!({ "url": server.url() }),
    )
    .await
    .unwrap();
    let serialized = serde_json::to_string(&out).unwrap();
    assert!(
        !serialized.contains("EVAL_SECRET_BODY_7f3a"),
        "bodies never surface"
    );
    assert!(out["evidence"]
        .as_array()
        .unwrap()
        .iter()
        .all(|e| e["value"]["body_bytes"].is_null() || e["value"]["body_bytes"] == 0));
    assert_bounded_envelope(&out);
}

// ---------------------------------------------------------------------------
// Shared cross-scenario integrity guard
// ---------------------------------------------------------------------------

/// One shared scenario: the full registry is healthy under eval conditions.
#[tokio::test]
async fn scenario_17_registry_integrity_under_eval_state() {
    let state = eval_state(default_config(), ScenarioBackend::with_fixtures());
    let names = state.tools.names();
    assert!(!names.is_empty());
    assert!(names.contains(&"diagnose_workspace".to_string()));
    assert!(names.contains(&"system_diagnose".to_string()));
    let mut sorted = names.clone();
    sorted.sort();
    assert_eq!(names, sorted, "registry names are deterministic");
    let unique: std::collections::HashSet<&String> = names.iter().collect();
    assert_eq!(unique.len(), names.len(), "no tool is registered twice");
}

/// Compile-time-ish check that the eval scenarios build an AppState exactly
/// like the rest of the test suite (guards against drift in helpers).
#[allow(dead_code)]
fn _unused_type_check(state: &Arc<AppState>) {
    let _: &Config = &state.config;
}

// ---------------------------------------------------------------------------
// Scenario 19 — hardware evidence drives system_diagnose
// ---------------------------------------------------------------------------

/// A backend whose hardware evidence is degraded across all four new domains:
/// high CPU thermal pressure with likely throttling, critical NVMe health,
/// a worn battery, and a weak Wi-Fi signal.
fn degraded_hardware_backend() -> ScenarioBackend {
    use winkit::models::{
        BatteryHealth, BatteryStatus, DiskHealthReport, SensorAvailability, StorageHealthDevice,
        ThermalSnapshot, ThermalStateSummary, UnavailableReading, WifiAdapterStatus,
    };

    let thermal = ThermalSnapshot {
        status: "degraded".into(),
        timestamp: "2026-08-13T08:00:02.000Z".into(),
        duration_ms: 15,
        sensors: vec![winkit::models::SensorReading::available(
            "thermal_zone-0",
            "Thermal zone 0",
            winkit::models::SensorClass::CpuPackage,
            winkit::models::SensorKind::Temperature,
            "0",
            96.0,
            "temperature_c",
            winkit::models::SensorSource::ThermalZone,
            winkit::models::SensorQuality::High,
            None,
            None,
        )],
        thermal_state: ThermalStateSummary {
            cpu_throttling: "likely".into(),
            gpu_throttling: "unknown".into(),
            cpu_thermal_pressure: "high".into(),
            gpu_thermal_pressure: "unknown".into(),
            cpu_frequency_reduced: Some(true),
            evidence: vec![winkit::models::EvidencePoint {
                metric: "cpu_temperature_c".into(),
                value: "96.0 C".into(),
                detail: "ACPI thermal zone temperature".into(),
            }],
            limitations: vec![],
        },
        completeness: "full".into(),
        unavailable: vec![UnavailableReading::new(
            "gpu",
            "temperature",
            SensorAvailability::Unsupported,
            "no documented Windows API exposes GPU temperature without a vendor SDK",
        )],
        warnings: Vec::new(),
    };

    let disk_health = DiskHealthReport {
        status: "critical".into(),
        timestamp: "2026-08-13T08:00:05.000Z".into(),
        duration_ms: 20,
        devices: vec![StorageHealthDevice {
            device: "PhysicalDrive0".into(),
            model: Some("Samsung SSD 980 PRO 1TB".into()),
            interface: "nvme".into(),
            health_status: Some("critical".into()),
            temperature_c: Some(41.0),
            critical_warning: vec!["reliability_degraded".into()],
            percentage_used: Some(96),
            available_spare: Some(3),
            available_spare_threshold: Some(10),
            media_errors: Some(12),
            power_on_hours: Some(9_800),
            unsafe_shutdowns: Some(31),
            data_units_read: Some(5_000_000),
            data_units_written: Some(4_200_000),
            reallocated_sectors: None,
            media_type: Some("ssd".into()),
            bus_type: Some("nvme".into()),
            firmware_version: Some("4B2QGXA7".into()),
            serial_number: Some("S680NF0R123456".into()),
            physical_location: None,
            spindle_speed_rpm: None,
            availability: SensorAvailability::Available,
            reason: None,
        }],
        completeness: "full".into(),
        unavailable: Vec::new(),
    };

    let battery_status = BatteryStatus {
        status: "ok".into(),
        timestamp: "2026-08-13T08:00:03.000Z".into(),
        present: true,
        percent: Some(38),
        ac_online: Some(false),
        charging: Some(false),
        battery_state: Some("discharging".into()),
        estimated_time_remaining_seconds: Some(4_200),
        health: Some(BatteryHealth {
            designed_capacity_mwh: Some(90_000),
            full_charge_capacity_mwh: Some(36_000),
            current_charge_mwh: Some(13_680),
            cycle_count: Some(820),
            health_percent: Some(40.0),
            temperature_c: None,
            availability: SensorAvailability::Available,
            reason: None,
        }),
        unavailable: Vec::new(),
    };

    let wifi = vec![WifiAdapterStatus {
        adapter_id: "{11111111-2222-3333-4444-555555555555}".into(),
        description: "Intel(R) Wi-Fi 6 AX210".into(),
        state: "connected".into(),
        ssid: Some("HomeNet".into()),
        signal_percent: Some(12),
        rssi_dbm: Some(-88),
        link_speed_mbps: Some(24.0),
        channel: Some(36),
        frequency_mhz: Some(5180),
        band: Some("5ghz".into()),
        authentication: Some("wpa2_psk".into()),
        cipher: Some("ccmp".into()),
        is_up: true,
    }];

    let mut backend = ScenarioBackend::with_fixtures();
    backend.thermal = Some(thermal);
    backend.disk_health = Some(disk_health);
    backend.battery_status = Some(battery_status);
    backend.wifi_status_override = Some(wifi);
    backend
}

#[tokio::test]
async fn scenario_19_hardware_evidence_feeds_system_diagnose() {
    let state = eval_state(default_config(), degraded_hardware_backend());

    // The hardware tools themselves stay bounded and structured.
    let hardware = call(&state, "hardware_snapshot", json!({})).await.unwrap();
    assert_eq!(hardware["status"], "ok");
    assert!(hardware["cpu"]["package_temperature_c"].is_number());
    assert_no_root_cause_claims(&hardware);

    let thermal = call(&state, "thermal_snapshot", json!({})).await.unwrap();
    assert_eq!(thermal["thermal_state"]["cpu_thermal_pressure"], "high");
    assert_eq!(thermal["thermal_state"]["cpu_throttling"], "likely");
    assert_no_root_cause_claims(&thermal);

    let disk = call(&state, "disk_health", json!({})).await.unwrap();
    assert_eq!(disk["status"], "critical");
    assert_eq!(disk["devices"][0]["health_status"], "critical");

    let battery = call(&state, "battery_status", json!({})).await.unwrap();
    assert_eq!(battery["health"]["health_percent"], 40.0);

    let wifi = call(&state, "wifi_status", json!({})).await.unwrap();
    assert_eq!(wifi["count"], 1);
    assert_eq!(wifi["adapters"][0]["signal_percent"], 12);

    // system_diagnose consumes the degraded hardware evidence and emits
    // cross-domain findings instead of ignoring it.
    let diagnose = call(&state, "system_diagnose", json!({})).await.unwrap();
    assert_eq!(
        diagnose["diagnosis"]["report"]["status"],
        "signals_detected"
    );
    let findings = diagnose["diagnosis"]["findings"].as_array().unwrap();
    let categories: Vec<&str> = findings
        .iter()
        .filter_map(|f| f["category"].as_str())
        .collect();
    for expected in ["thermal", "storage_health", "battery", "wifi"] {
        assert!(
            categories.contains(&expected),
            "expected a '{expected}' finding, got {categories:?}"
        );
    }
    // A measured, failing dimension is never listed as checked clean.
    let clean = diagnose["diagnosis"]["checked_clean"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|v| v.as_str())
        .collect::<Vec<_>>();
    for dim in [
        "CPU thermal state",
        "storage health",
        "battery health",
        "Wi-Fi signal strength",
    ] {
        assert!(
            !clean.contains(&dim),
            "'{dim}' failed its threshold and must not be checked clean"
        );
    }
    // Findings stay evidence-backed and bounded, and never overclaim.
    let measurements = diagnose["diagnosis"]["report"]["measurements"]
        .as_array()
        .unwrap();
    let metrics: Vec<&str> = measurements
        .iter()
        .filter_map(|m| m["metric"].as_str())
        .collect();
    for metric in [
        "cpu_thermal_pressure",
        "drive_health_status",
        "battery_health_percent",
        "wifi_signal_percent",
    ] {
        assert!(
            metrics.contains(&metric),
            "finding evidence must reference measurement '{metric}', got {metrics:?}"
        );
    }
    let serialized = serde_json::to_string(&diagnose).unwrap();
    assert!(serialized.len() <= 256 * 1024);
    assert_no_root_cause_claims(&diagnose);
}
