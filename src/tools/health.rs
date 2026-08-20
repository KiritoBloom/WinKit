//! Machine-wide health: `system_health` and `system_diagnose`.
//!
//! `system_health` aggregates per-application resource groups from the
//! Windows backend, adds system-level facts (memory pressure, disk space),
//! and applies configured thresholds to produce an explicit issue list
//! ranked by a deterministic score. `system_diagnose` runs the same evidence
//! through the diagnostic engine and returns ranked findings plus a
//! "checked clean" list, so the AI agent explains findings instead of
//! inventing causes.

use crate::config::{Config, HealthConfig};
use crate::diagnostics::findings::{
    app_cpu_score, memory_pressure_score, score_bands, storage_score,
};
use crate::errors::WinkitError;
use crate::models::{
    ApplicationGroupInfo, DriveHealth, DriveInfo, HealthIssue, ResourceSnapshot, SystemAppEvidence,
    SystemBatteryEvidence, SystemDiagnosticData, SystemDriveEvidence, SystemHealth,
    SystemHealthReport, SystemStorageHealthEvidence, SystemThermalEvidence, SystemWifiEvidence,
};
use crate::permissions::Capability;
use crate::server::AppState;
use crate::tools::{clamp_limit, optional_usize, wrap, ToolDefinition};
use serde_json::{json, Value};
use std::sync::Arc;
use std::time::Duration;

/// Apply configured thresholds and build the report (pure, testable).
/// Issues are sorted by deterministic score, descending.
pub fn build_health_report(
    mut groups: Vec<ApplicationGroupInfo>,
    resources: &ResourceSnapshot,
    drives: &[DriveInfo],
    config: &HealthConfig,
) -> SystemHealthReport {
    let mut issues: Vec<HealthIssue> = Vec::new();
    for g in &mut groups {
        let classification = crate::diagnostics::health::classify_application_group(
            g.cpu_percent,
            g.total_working_set_bytes,
            config,
        );
        g.status = classification.status().to_string();
        if classification.high_cpu {
            let cpu = g.cpu_percent.unwrap_or(0.0);
            let score = app_cpu_score(cpu);
            let (severity, _) = score_bands(score);
            issues.push(HealthIssue {
                layer: "application".into(),
                subject: g.display_name.clone(),
                kind: "high_cpu".into(),
                value: format!("{cpu:.1}% of system CPU capacity"),
                threshold: format!(">= {:.0}% of system CPU capacity", config.high_cpu_percent),
                score,
                category: "app_cpu".into(),
                severity: severity.into(),
            });
        }
        if classification.high_memory {
            let score = app_memory_score_issue(g.total_working_set_bytes, resources, config);
            let (severity, _) = score_bands(score);
            issues.push(HealthIssue {
                layer: "application".into(),
                subject: g.display_name.clone(),
                kind: "high_memory".into(),
                value: format!(
                    "{} MB total working set",
                    g.total_working_set_bytes / (1024 * 1024)
                ),
                threshold: format!(">= {} MB", config.high_memory_bytes / (1024 * 1024)),
                score,
                category: "app_memory".into(),
                severity: severity.into(),
            });
        }
    }

    let drive_health: Vec<DriveHealth> = drives
        .iter()
        .map(|d| {
            let low = d
                .free_bytes
                .map(|f| f <= config.low_disk_free_bytes)
                .unwrap_or(false);
            if low {
                let free_pct = match (d.free_bytes, d.total_bytes) {
                    (Some(f), Some(t)) if t > 0 => Some(f as f64 / t as f64 * 100.0),
                    _ => None,
                };
                let score = free_pct.map(storage_score).unwrap_or(100);
                let (severity, _) = score_bands(score);
                issues.push(HealthIssue {
                    layer: "system".into(),
                    subject: drive_label(&d.root),
                    kind: "low_disk_space".into(),
                    value: format!("{:.1} GB free", d.free_bytes.unwrap_or(0) as f64 / 1e9),
                    threshold: format!("<= {:.0} GB free", config.low_disk_free_bytes as f64 / 1e9),
                    score,
                    category: "storage".into(),
                    severity: severity.into(),
                });
            }
            DriveHealth {
                drive: drive_label(&d.root),
                total_bytes: d.total_bytes,
                free_bytes: d.free_bytes,
                percent_free: match (d.free_bytes, d.total_bytes) {
                    (Some(f), Some(t)) if t > 0 => Some(f as f64 / t as f64 * 100.0),
                    _ => None,
                },
                low_disk_space: low,
            }
        })
        .collect();

    let pressure = resources
        .memory_load_percent
        .map(|v| v >= config.high_memory_load_percent)
        .unwrap_or(false);
    if pressure {
        let load = resources.memory_load_percent.unwrap_or(0.0);
        let available_percent = match (
            resources.available_memory_bytes,
            resources.total_memory_bytes,
        ) {
            (Some(avail), Some(total)) if total > 0 => Some(avail as f64 / total as f64 * 100.0),
            _ => None,
        };
        let score = memory_pressure_score(load, config.high_memory_load_percent, available_percent);
        let (severity, _) = score_bands(score);
        issues.push(HealthIssue {
            layer: "system".into(),
            subject: "memory".into(),
            kind: "memory_pressure".into(),
            value: format!("{load:.1}% load"),
            threshold: format!(">= {:.0}% load", config.high_memory_load_percent),
            score,
            category: "memory_pressure".into(),
            severity: severity.into(),
        });
    }

    issues.sort_by(|a, b| {
        b.score
            .cmp(&a.score)
            .then_with(|| a.subject.cmp(&b.subject))
    });

    SystemHealthReport {
        generated_at: crate::utils::time::format_rfc3339(std::time::SystemTime::now()),
        applications: groups,
        system: SystemHealth {
            memory_load_percent: resources.memory_load_percent,
            total_memory_bytes: resources.total_memory_bytes,
            available_memory_bytes: resources.available_memory_bytes,
            memory_pressure: pressure,
            drives: drive_health,
        },
        issues,
    }
}

/// Application memory score for the health issue, reusing the shared formula.
fn app_memory_score_issue(
    working_set_bytes: u64,
    resources: &ResourceSnapshot,
    config: &HealthConfig,
) -> u8 {
    crate::diagnostics::findings::app_memory_score(
        working_set_bytes,
        resources.total_memory_bytes,
        config.high_memory_bytes,
    )
}

/// `C:\` -> `C:`.
fn drive_label(root: &str) -> String {
    root.trim_end_matches(['\\', '/']).to_string()
}

pub async fn system_health_handler(
    state: Arc<AppState>,
    args: Value,
) -> Result<Value, WinkitError> {
    let limit = clamp_limit(
        optional_usize(&args, "limit"),
        state.config.health.max_groups,
    );
    // `application_groups` samples CPU for 1 s and enumerates processes;
    // `resource_snapshot(1_000)` sleeps 1 s. Run all three on worker threads
    // so the stdio loop is never stalled by one slow health call.
    let (groups, resources, drives) = {
        let state = state.clone();
        tokio::task::spawn_blocking(move || {
            Ok::<_, WinkitError>((
                state.windows.application_groups(limit)?,
                state.windows.resource_snapshot(1_000)?,
                state.windows.list_drives()?,
            ))
        })
        .await
        .map_err(|e| WinkitError::internal(format!("health collection failed: {e}")))?
    }?;
    let report = build_health_report(groups, &resources, &drives, &state.config.health);
    Ok(serde_json::to_value(report)?)
}

pub fn system_health_definition(_config: &Config) -> ToolDefinition {
    ToolDefinition {
        name: "system_health",
        description: "Machine-wide health summary: running applications grouped by executable with aggregate memory and CPU (sampled), system memory pressure, and disk space — with explicit threshold-based issues ranked by a deterministic score. Use it to answer 'what is currently unhealthy on this machine' in one call.",
        input_schema: json!({
            "type": "object",
            "properties": {
                "limit": { "type": "integer", "description": "Maximum application groups to return, by total working set (default and cap come from configuration)." },
            },
            "additionalProperties": false,
        }),
        capability: Some(Capability::ProcessRead),
        timeout_ms: None,
        handler: wrap(system_health_handler),
    }
}

// system_diagnose

pub async fn system_diagnose_handler(
    state: Arc<AppState>,
    args: Value,
) -> Result<Value, WinkitError> {
    let limit = clamp_limit(
        optional_usize(&args, "limit"),
        state.config.health.max_groups,
    );
    // Evidence collection is best-effort per dimension: a failed collection
    // yields a `limited` report instead of a failed tool call. The
    // application-group enumeration samples CPU for 1 s, so it runs on a
    // worker thread to keep the stdio loop responsive.
    let (groups, drives) = {
        let state = state.clone();
        tokio::task::spawn_blocking(move || {
            (
                state.windows.application_groups(limit).unwrap_or_default(),
                state.windows.list_drives().unwrap_or_default(),
            )
        })
        .await
        .map_err(|e| WinkitError::internal(format!("health collection failed: {e}")))?
    };

    // Bounded hardware evidence: each probe runs under the
    // configured budget and degrades to `None`/empty instead of failing.
    let budget = state.config.hardware.probe_timeout_ms;
    let thermal_state = state.clone();
    let thermal =
        crate::tools::hardware::probe(budget, move || thermal_state.windows.thermal_snapshot())
            .await
            .ok()
            .map(|r| {
                let mut temps: Vec<f64> = r
                    .sensors
                    .iter()
                    .filter(|s| s.availability.is_available())
                    .filter(|s| {
                        matches!(
                            s.class,
                            crate::models::SensorClass::CpuPackage
                                | crate::models::SensorClass::CpuCore
                        )
                    })
                    .filter_map(|s| s.value)
                    .collect();
                temps.sort_by(|a, b| a.total_cmp(b));
                SystemThermalEvidence {
                    cpu_thermal_pressure: r.thermal_state.cpu_thermal_pressure.clone(),
                    cpu_throttling: r.thermal_state.cpu_throttling.clone(),
                    cpu_frequency_reduced: r.thermal_state.cpu_frequency_reduced,
                    cpu_temperature_c: temps.last().copied(),
                    gpu_thermal_pressure: r.thermal_state.gpu_thermal_pressure.clone(),
                }
            });
    let storage_state = state.clone();
    let storage_health: Vec<SystemStorageHealthEvidence> =
        crate::tools::hardware::probe(budget, move || storage_state.windows.disk_health())
            .await
            .ok()
            .map(|r| {
                r.devices
                    .into_iter()
                    .map(|d| SystemStorageHealthEvidence {
                        device: d.device,
                        interface: d.interface,
                        health_status: d.health_status,
                        temperature_c: d.temperature_c,
                        percentage_used: d.percentage_used,
                    })
                    .collect()
            })
            .unwrap_or_default();
    let battery_state = state.clone();
    let battery =
        crate::tools::hardware::probe(budget, move || battery_state.windows.battery_status())
            .await
            .ok()
            .map(|b| SystemBatteryEvidence {
                present: b.present,
                percent: b.percent,
                ac_online: b.ac_online,
                charging: b.charging,
                battery_state: b.battery_state,
                health_percent: b.health.as_ref().and_then(|h| h.health_percent),
            });
    let wifi_state = state.clone();
    let wifi: Vec<SystemWifiEvidence> =
        crate::tools::hardware::probe(budget, move || wifi_state.windows.wifi_status())
            .await
            .ok()
            .map(|adapters| {
                adapters
                    .into_iter()
                    .map(|a| SystemWifiEvidence {
                        description: a.description,
                        state: a.state,
                        signal_percent: a.signal_percent,
                        link_speed_mbps: a.link_speed_mbps,
                    })
                    .collect()
            })
            .unwrap_or_default();

    let start = std::time::Instant::now();
    let snap_a = state.windows.resource_snapshot(0).ok();
    tokio::time::sleep(Duration::from_millis(1_000)).await;
    let snap_b = state.windows.resource_snapshot(0).ok();
    let elapsed_ms = start.elapsed().as_millis().max(1) as i64;

    let memory_load_percent = snap_b
        .as_ref()
        .and_then(|s| s.memory_load_percent)
        .or_else(|| snap_a.as_ref().and_then(|s| s.memory_load_percent));
    let memory_available_bytes = snap_b
        .as_ref()
        .and_then(|s| s.available_memory_bytes)
        .or_else(|| snap_a.as_ref().and_then(|s| s.available_memory_bytes));
    let memory_total_bytes = snap_b
        .as_ref()
        .and_then(|s| s.total_memory_bytes)
        .or_else(|| snap_a.as_ref().and_then(|s| s.total_memory_bytes));
    let memory_growth_bytes_per_second = match (snap_a.as_ref(), snap_b.as_ref()) {
        (Some(a), Some(b)) => match (a.available_memory_bytes, b.available_memory_bytes) {
            (Some(x), Some(y)) => Some((x as i64 - y as i64) * 1000 / elapsed_ms),
            _ => None,
        },
        _ => None,
    };

    let data = SystemDiagnosticData {
        memory_load_percent,
        memory_available_bytes,
        memory_total_bytes,
        memory_growth_bytes_per_second,
        drives: drives
            .iter()
            .filter_map(|d| {
                let free_bytes = d.free_bytes?;
                let total_bytes = d.total_bytes?;
                Some(SystemDriveEvidence {
                    subject: drive_label(&d.root),
                    free_bytes,
                    total_bytes,
                })
            })
            .collect(),
        app_groups: groups
            .iter()
            .map(|g| SystemAppEvidence {
                name: g.name.clone(),
                display_name: g.display_name.clone(),
                process_count: g.process_count,
                tree_process_count: g.tree_process_count,
                working_set_bytes: g.total_working_set_bytes,
                own_working_set_bytes: g.own_working_set_bytes,
                cpu_percent: g.cpu_percent,
            })
            .collect(),
        thermal,
        storage_health,
        battery,
        wifi,
    };

    let diagnosis = crate::diagnostics::system::analyze_system(
        &data,
        &state.config.diagnostics,
        &state.config.health,
    );
    Ok(json!({
        "diagnosis": diagnosis,
        "applications": groups,
        "drives": drives,
    }))
}

pub fn system_diagnose_definition(_config: &Config) -> ToolDefinition {
    ToolDefinition {
        name: "system_diagnose",
        description: "Machine-wide diagnosis: gathers application, storage, memory, memory-growth, thermal, storage-health, battery, and Wi-Fi evidence, applies deterministic threshold rules, and returns ranked findings with explicit scores plus a 'checked clean' list of dimensions that were measured and found healthy. Use it to answer 'why is my computer unhealthy' — findings are evidence-backed hypotheses, not root-cause claims.",
        input_schema: json!({
            "type": "object",
            "properties": {
                "limit": { "type": "integer", "description": "Maximum application groups to consider, by total working set (default and cap come from configuration)." },
            },
            "additionalProperties": false,
        }),
        capability: Some(Capability::ProcessRead),
        timeout_ms: None,
        handler: wrap(system_diagnose_handler),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::{DriveInfo, ResourceSnapshot};

    fn group(name: &str, ws_mb: u64, cpu: Option<f64>) -> ApplicationGroupInfo {
        ApplicationGroupInfo {
            name: name.into(),
            display_name: name.into(),
            process_count: 1,
            tree_process_count: 1,
            total_working_set_bytes: ws_mb * 1024 * 1024,
            own_working_set_bytes: ws_mb * 1024 * 1024,
            cpu_percent: cpu,
            cpu_percent_basis: "system_capacity_all_cores".into(),
            cpu_percent_sample_ms: 300,
            status: "normal".into(),
        }
    }

    fn snapshot(load: f64, total: u64, available: u64) -> ResourceSnapshot {
        ResourceSnapshot {
            cpu_busy_percent: None,
            cpu_busy_percent_basis: "system_capacity_all_cores".into(),
            memory_load_percent: Some(load),
            total_memory_bytes: Some(total),
            available_memory_bytes: Some(available),
        }
    }

    #[test]
    fn flags_high_cpu_and_high_memory_with_thresholds() {
        let groups = vec![
            group("chrome", 3_500, Some(45.0)),
            group("node", 200, Some(5.0)),
        ];
        let cfg = HealthConfig::default();
        let report = build_health_report(
            groups,
            &snapshot(40.0, 16_000_000_000, 9_600_000_000),
            &[],
            &cfg,
        );
        assert_eq!(report.applications[0].status, "high_cpu_and_memory");
        assert_eq!(report.applications[1].status, "normal");
        assert_eq!(report.issues.len(), 2);
        assert!(report.issues.iter().any(|i| i.kind == "high_cpu"));
        assert!(report.issues.iter().any(|i| i.kind == "high_memory"));
        // Every issue carries a deterministic score and category.
        for issue in &report.issues {
            assert!(issue.score > 0);
            assert!(!issue.category.is_empty());
        }
        // The high-memory issue (3.5 GB of a 16 GB machine) is the top issue.
        assert_eq!(report.issues[0].kind, "high_memory");
    }

    #[test]
    fn flags_low_disk_space_and_memory_pressure() {
        let cfg = HealthConfig::default();
        let report = build_health_report(
            Vec::new(),
            &snapshot(92.0, 16_000_000_000, 1_280_000_000),
            &[DriveInfo {
                root: "C:\\".into(),
                kind: "fixed".into(),
                total_bytes: Some(500_000_000_000),
                free_bytes: Some(5_000_000_000),
                used_bytes: Some(495_000_000_000),
                percent_used: Some(99.0),
            }],
            &cfg,
        );
        assert!(report.system.memory_pressure);
        assert!(report.system.drives[0].low_disk_space);
        assert_eq!(report.issues.len(), 2);
        // 1% free scores 100 -> critical storage pressure ranks first.
        let storage = report
            .issues
            .iter()
            .find(|i| i.kind == "low_disk_space")
            .unwrap();
        assert_eq!(storage.severity, "critical");
        assert_eq!(storage.score, 100);
        assert_eq!(storage.category, "storage");
        assert_eq!(report.issues[0].kind, "low_disk_space");
        assert!(report.issues.iter().any(|i| i.kind == "memory_pressure"));
    }

    #[test]
    fn quiet_machine_reports_no_issues() {
        let cfg = HealthConfig::default();
        let report = build_health_report(
            vec![group("node", 100, Some(1.0))],
            &snapshot(30.0, 16_000_000_000, 11_200_000_000),
            &[DriveInfo {
                root: "C:\\".into(),
                kind: "fixed".into(),
                total_bytes: Some(500_000_000_000),
                free_bytes: Some(200_000_000_000),
                used_bytes: Some(300_000_000_000),
                percent_used: Some(60.0),
            }],
            &cfg,
        );
        assert!(report.issues.is_empty());
        assert_eq!(report.applications[0].status, "normal");
        assert!(!report.system.drives[0].low_disk_space);
    }

    #[test]
    fn drive_label_normalizes_trailing_separator() {
        assert_eq!(drive_label("C:\\"), "C:");
        assert_eq!(drive_label("D:/"), "D:");
    }

    #[test]
    fn issues_are_sorted_by_score_descending() {
        let cfg = HealthConfig::default();
        let report = build_health_report(
            vec![group("chrome", 4_000, Some(12.0))],
            &snapshot(88.0, 16_000_000_000, 1_900_000_000),
            &[DriveInfo {
                root: "C:\\".into(),
                kind: "fixed".into(),
                total_bytes: Some(500_000_000_000),
                free_bytes: Some(3_000_000_000),
                used_bytes: Some(497_000_000_000),
                percent_used: Some(99.4),
            }],
            &cfg,
        );
        let scores: Vec<u8> = report.issues.iter().map(|i| i.score).collect();
        let mut sorted = scores.clone();
        sorted.sort_by(|a, b| b.cmp(a));
        assert_eq!(scores, sorted);
    }

    #[test]
    fn diagnose_with_mock_evidence_ranks_findings() {
        let data = SystemDiagnosticData {
            memory_load_percent: Some(92.0),
            memory_available_bytes: Some(1_280_000_000),
            memory_total_bytes: Some(16_000_000_000),
            memory_growth_bytes_per_second: Some(2 * 1024 * 1024),
            drives: vec![SystemDriveEvidence {
                subject: "C:".into(),
                free_bytes: 5_000_000_000,
                total_bytes: 500_000_000_000,
            }],
            app_groups: vec![SystemAppEvidence {
                name: "chrome".into(),
                display_name: "Google Chrome".into(),
                process_count: 3,
                tree_process_count: 8,
                working_set_bytes: 3_500 * 1024 * 1024,
                own_working_set_bytes: 1_200 * 1024 * 1024,
                cpu_percent: Some(45.0),
            }],
            ..SystemDiagnosticData::default()
        };
        let diagnosis = crate::diagnostics::system::analyze_system(
            &data,
            &crate::config::DiagnosticsConfig::default(),
            &cfg_default(),
        );
        assert!(!diagnosis.findings.is_empty());
        assert_eq!(diagnosis.findings[0].category, "storage");
        assert!(diagnosis.report.measurements.len() >= 6);
        assert!(diagnosis
            .report
            .possible_causes
            .iter()
            .any(|c| c.hypothesis.contains("storage pressure")));
    }

    fn cfg_default() -> HealthConfig {
        HealthConfig::default()
    }
}
