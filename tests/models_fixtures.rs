//! Fixture round-trip tests: every synthetic fixture must deserialize into
//! the corresponding WinKit model. Run with `cargo test --features mocks`.

use winkit::models::*;

fn load(name: &str) -> String {
    std::fs::read_to_string(format!(
        "{}/tests/fixtures/{name}",
        env!("CARGO_MANIFEST_DIR")
    ))
    .expect("fixture should exist")
}

#[test]
fn processes_fixture_deserializes() {
    let processes: Vec<ProcessInfo> = serde_json::from_str(&load("processes.json")).unwrap();
    assert_eq!(processes.len(), 7);
    let chrome = processes.iter().find(|p| p.pid == 420).unwrap();
    assert_eq!(chrome.name, "chrome.exe");
    assert_eq!(chrome.parent_pid, Some(4));
    assert_eq!(chrome.working_set_bytes, Some(1_900_000_000));
    let node = processes.iter().find(|p| p.pid == 900).unwrap();
    assert!(node.command_line.as_deref().unwrap().contains("--watch"));
}

#[test]
fn ports_fixture_deserializes() {
    let ports: Vec<PortInfo> = serde_json::from_str(&load("ports.json")).unwrap();
    assert_eq!(ports.len(), 3);
    let dev = ports.iter().find(|p| p.port == 3000).unwrap();
    assert_eq!(dev.process_name.as_deref(), Some("node.exe"));
    assert_eq!(dev.pid, Some(900));
}

#[test]
fn services_fixture_deserializes() {
    let services: Vec<ServiceInfo> = serde_json::from_str(&load("services.json")).unwrap();
    assert_eq!(services.len(), 3);
    let spooler = services.iter().find(|s| s.name == "Spooler").unwrap();
    assert_eq!(spooler.state, "running");
    assert_eq!(spooler.start_type.as_deref(), Some("auto"));
}

#[test]
fn events_fixture_deserializes_with_levels() {
    let events: Vec<EventInfo> = serde_json::from_str(&load("events.json")).unwrap();
    assert_eq!(events.len(), 3);
    assert_eq!(events[0].level, EventLevel::Error);
    assert_eq!(events[1].level, EventLevel::Information);
    assert_eq!(events[2].level, EventLevel::Warning);
    assert_eq!(events[0].channel.as_deref(), Some("Application"));
}

#[test]
fn tabs_fixture_deserializes() {
    let tabs: Vec<TabInfo> = serde_json::from_str(&load("tabs.json")).unwrap();
    assert_eq!(tabs.len(), 3);
    let heavy = tabs.iter().find(|t| t.id == "tab-001").unwrap();
    assert!(heavy.active);
    assert_eq!(heavy.kind, "page");
}

#[test]
fn chrome_performance_fixture_deserializes() {
    let p: PerformanceMetrics = serde_json::from_str(&load("chrome_performance.json")).unwrap();
    assert_eq!(p.long_task_ms, 2400.0);
    assert_eq!(p.script_ms, 3100.0);
    assert!(p.metrics.contains_key("JSHeapUsedSize"));
    assert!(p.deltas.contains_key("TaskDuration"));
}

#[test]
fn chrome_memory_fixture_deserializes() {
    let m: MemoryInfo = serde_json::from_str(&load("chrome_memory.json")).unwrap();
    assert_eq!(m.js_heap_used_bytes, Some(734_003_200));
    assert_eq!(m.dom_nodes, Some(90_000));
    assert_eq!(m.growth_rate_bytes_per_second, Some(4_194_304));
}

#[test]
fn chrome_diagnostics_fixture_deserializes() {
    let report: DiagnosticReport = serde_json::from_str(&load("chrome_diagnostics.json")).unwrap();
    let kinds: Vec<&str> = report.signals.iter().map(|s| s.kind.as_str()).collect();
    assert!(kinds.contains(&"high_cpu"));
    assert!(kinds.contains(&"high_memory"));
    assert!(kinds.contains(&"rapid_heap_growth"));
    assert!(report
        .possible_causes
        .iter()
        .any(|c| c.hypothesis.contains("JavaScript pressure")));
    assert!(!report.limitations.is_empty());
    assert_eq!(report.status, "signals_detected");
    assert_eq!(report.evidence_completeness, "full");
    // Evidence-first shape: measurements are facts, separate from signals.
    assert!(!report.measurements.is_empty());
    assert!(report
        .measurements
        .iter()
        .any(|m| m.metric == "cpu_percent"));
    let metrics: Vec<&str> = report
        .measurements
        .iter()
        .map(|m| m.metric.as_str())
        .collect();
    for signal in &report.signals {
        for e in &signal.evidence {
            assert!(
                metrics.contains(&e.metric.as_str()),
                "signal '{}' references metric '{}' with no measurement",
                signal.kind,
                e.metric
            );
        }
    }
    assert!(report
        .agent_guidance
        .contains("heuristics over measured evidence"));
}
