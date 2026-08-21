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
