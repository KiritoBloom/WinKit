//! Machine-wide diagnosis (§77): system evidence → ranked findings.
//!
//! `analyze_system` takes machine-wide evidence (memory, drives, application
//! groups, memory growth), applies the same evidence-first discipline as tab
//! diagnosis — measurements are reported separately from signals — and adds a
//! deterministic ranking of findings plus a "checked clean" list. It is a
//! pure, testable function: no Windows calls, no LLM.

use crate::config::{DiagnosticsConfig, HealthConfig};
use crate::diagnostics::findings::{
    app_cpu_score, app_memory_score, battery_health_score, frequency_reduction_score,
    memory_growth_score, memory_pressure_score, score_bands, storage_health_score, storage_score,
    thermal_pressure_score, wifi_signal_score,
};
use crate::models::{
    DiagnosticReport, DiagnosticSignal, EvidencePoint, Measurement, PossibleCause, RankedFinding,
    SystemDiagnosticData,
};
use crate::utils::time;

/// System-level signal kinds (§77).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SystemSignalKind {
    StoragePressure,
    MemoryPressure,
    AppHighCpu,
    AppHighMemory,
    MemoryGrowth,
    CpuThermalPressure,
    CpuFrequencyReduced,
    StorageHealth,
    BatteryDegraded,
    WifiWeakSignal,
}

impl SystemSignalKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::StoragePressure => "storage_pressure",
            Self::MemoryPressure => "memory_pressure",
            Self::AppHighCpu => "app_high_cpu",
            Self::AppHighMemory => "app_high_memory",
            Self::MemoryGrowth => "memory_growth",
            Self::CpuThermalPressure => "cpu_thermal_pressure",
            Self::CpuFrequencyReduced => "cpu_frequency_reduced",
            Self::StorageHealth => "storage_health",
            Self::BatteryDegraded => "battery_degraded",
            Self::WifiWeakSignal => "wifi_weak_signal",
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::StoragePressure => "Storage pressure",
            Self::MemoryPressure => "System memory pressure",
            Self::AppHighCpu => "High application CPU usage",
            Self::AppHighMemory => "High application memory usage",
            Self::MemoryGrowth => "Runaway memory growth",
            Self::CpuThermalPressure => "CPU thermal pressure",
            Self::CpuFrequencyReduced => "CPU frequency reduced",
            Self::StorageHealth => "Storage health concern",
            Self::BatteryDegraded => "Battery degraded",
            Self::WifiWeakSignal => "Weak Wi-Fi signal",
        }
    }

    fn severity(self) -> &'static str {
        match self {
            Self::StoragePressure => "high",
            Self::MemoryPressure => "high",
            Self::AppHighCpu => "medium",
            Self::AppHighMemory => "medium",
            Self::MemoryGrowth => "high",
            Self::CpuThermalPressure => "high",
            Self::CpuFrequencyReduced => "medium",
            Self::StorageHealth => "high",
            Self::BatteryDegraded => "medium",
            Self::WifiWeakSignal => "low",
        }
    }
}

/// Run the machine-wide diagnosis over the measured evidence.
pub fn analyze_system(
    data: &SystemDiagnosticData,
    diagnostics: &DiagnosticsConfig,
    health: &HealthConfig,
) -> crate::models::SystemDiagnosis {
    let measurements = system_measurements(data);
    let (signals, mut findings) = system_signals_and_findings(data, health, diagnostics);

    findings.sort_by(|a, b| {
        b.score
            .cmp(&a.score)
            .then_with(|| a.subject.cmp(&b.subject))
    });
    for (i, f) in findings.iter_mut().enumerate() {
        f.rank = i + 1;
    }

    let checked_clean = checked_clean(data, &signals);
    let status = if signals.is_empty() {
        "no_supported_signal_detected"
    } else {
        "signals_detected"
    };
    let evidence_completeness = if data.memory_load_percent.is_none() || data.app_groups.is_empty()
    {
        "limited"
    } else {
        "full"
    };
    let agent_guidance = if signals.is_empty() {
        "No supported evidence of machine-wide unhealth was found. Do not infer a cause from resource usage alone."
    } else {
        "Findings are deterministic rankings over measured evidence; treat them as hypotheses, not verified root causes."
    };

    let report = DiagnosticReport {
        target: "windows machine".to_string(),
        generated_at: time::format_rfc3339(std::time::SystemTime::now()),
        status: status.to_string(),
        evidence_completeness: evidence_completeness.to_string(),
        measurements,
        signals,
        // System scope ranks findings instead of correlating signal pairs.
        correlations: Vec::new(),
        possible_causes: findings
            .iter()
            .map(finding_as_possible_cause)
            .collect(),
        limitations: vec![
            "Thresholds are heuristic defaults from the health and diagnostics configuration; they are not guarantees of a problem.".to_string(),
            "CPU percentages are relative to total system CPU capacity across all logical processors; 100% means every core is fully busy, and a single busy core on an N-core machine reports 100/N percent.".to_string(),
            "Memory growth is sampled over a short window (~1 s) and may miss slower trends.".to_string(),
            "Thermal, storage-health, battery, and Wi-Fi evidence is a single point-in-time sample; ATA S.M.A.R.T. health may be unavailable without elevation.".to_string(),
        ],
        agent_guidance: agent_guidance.to_string(),
    };

    crate::models::SystemDiagnosis {
        report,
        findings,
        checked_clean,
    }
}

/// Convert every raw measured fact into a [`Measurement`], separate from any
/// interpretation. Zero-valued counters are omitted: absence of a signal is
/// the honest negative statement, and raw sections (e.g. the diagnose tool's
/// `runtime` block) already carry exact zeros.
fn system_measurements(data: &SystemDiagnosticData) -> Vec<Measurement> {
    let mut out = Vec::new();
    if let Some(v) = data.memory_load_percent {
        out.push(Measurement {
            metric: "memory_load_percent".into(),
            value: format!("{v:.1}%"),
            value_number: Some(v),
            unit: "percent".into(),
            scope: "system".into(),
            subject: None,
            detail: "System memory load reported by GlobalMemoryStatusEx; 100% = physical RAM exhausted.".into(),
        });
    }
    if let Some(v) = data.memory_available_bytes {
        out.push(Measurement {
            metric: "memory_available_bytes".into(),
            value: format!("{:.1} GB", v as f64 / 1e9),
            value_number: Some(v as f64),
            unit: "bytes".into(),
            scope: "system".into(),
            subject: None,
            detail: "Available physical memory.".into(),
        });
    }
    if let Some(v) = data.memory_total_bytes {
        out.push(Measurement {
            metric: "memory_total_bytes".into(),
            value: format!("{:.1} GB", v as f64 / 1e9),
            value_number: Some(v as f64),
            unit: "bytes".into(),
            scope: "system".into(),
            subject: None,
            detail: "Total physical memory.".into(),
        });
    }
    if let Some(v) = data.memory_growth_bytes_per_second {
        out.push(Measurement {
            metric: "memory_growth_bytes_per_second".into(),
            value: format!("{:.1} MB/s", v as f64 / (1024.0 * 1024.0)),
            value_number: Some(v as f64),
            unit: "bytes_per_second".into(),
            scope: "system".into(),
            subject: None,
            detail: "Rate of decrease in available memory across two samples ~1 s apart; positive = memory being consumed.".into(),
        });
    }
    for d in &data.drives {
        out.push(Measurement {
            metric: "drive_free_bytes".into(),
            value: format!("{:.1} GB", d.free_bytes as f64 / 1e9),
            value_number: Some(d.free_bytes as f64),
            unit: "bytes".into(),
            scope: "system".into(),
            subject: Some(d.subject.clone()),
            detail: "Free space reported for this drive.".into(),
        });
        let pct = percent_free(d);
        out.push(Measurement {
            metric: "drive_free_percent".into(),
            value: format!("{pct:.1}%"),
            value_number: Some(pct),
            unit: "percent".into(),
            scope: "system".into(),
            subject: Some(d.subject.clone()),
            detail: "Free space as a percentage of total drive capacity.".into(),
        });
    }
    for g in &data.app_groups {
        out.push(Measurement {
            metric: "working_set_bytes".into(),
            value: format!("{:.1} GB", g.working_set_bytes as f64 / 1e9),
            value_number: Some(g.working_set_bytes as f64),
            unit: "bytes".into(),
            scope: "application".into(),
            subject: Some(g.display_name.clone()),
            detail: "Sum of working set across all processes of this executable.".into(),
        });
        if let Some(v) = g.cpu_percent {
            out.push(Measurement {
                metric: "cpu_percent".into(),
                value: format!("{v:.1}% of system CPU capacity"),
                value_number: Some(v),
                unit: "percent_of_system_cpu_capacity".into(),
                scope: "application".into(),
                subject: Some(g.display_name.clone()),
                detail: "Aggregate process CPU time over total system CPU time; 100% = all logical processors fully busy.".into(),
            });
        }
    }
    if let Some(t) = &data.thermal {
        out.push(Measurement {
            metric: "cpu_thermal_pressure".into(),
            value: t.cpu_thermal_pressure.clone(),
            value_number: None,
            unit: "enum".into(),
            scope: "system".into(),
            subject: Some("cpu_package".into()),
            detail: "Interpreted thermal state: low, elevated, high, or unknown.".into(),
        });
        out.push(Measurement {
            metric: "cpu_throttling".into(),
            value: t.cpu_throttling.clone(),
            value_number: None,
            unit: "enum".into(),
            scope: "system".into(),
            subject: Some("cpu_package".into()),
            detail: "Interpreted throttling state: likely, not_observed, or unknown.".into(),
        });
        if let Some(c) = t.cpu_temperature_c {
            out.push(Measurement {
                metric: "cpu_temperature_c".into(),
                value: format!("{c:.1} C"),
                value_number: Some(c),
                unit: "celsius".into(),
                scope: "system".into(),
                subject: Some("cpu_package".into()),
                detail: "Highest readable CPU temperature sensor.".into(),
            });
        }
        if let Some(r) = t.cpu_frequency_reduced {
            out.push(Measurement {
                metric: "cpu_frequency_reduced".into(),
                value: r.to_string(),
                value_number: Some(if r { 1.0 } else { 0.0 }),
                unit: "boolean".into(),
                scope: "system".into(),
                subject: Some("cpu_package".into()),
                detail: "Current clock below base clock, when the frequency sensor exists.".into(),
            });
        }
    }
    for d in &data.storage_health {
        if let Some(s) = &d.health_status {
            out.push(Measurement {
                metric: "drive_health_status".into(),
                value: s.clone(),
                value_number: None,
                unit: "enum".into(),
                scope: "system".into(),
                subject: Some(d.device.clone()),
                detail: "Reported S.M.A.R.T. health status.".into(),
            });
        }
        if let Some(c) = d.temperature_c {
            out.push(Measurement {
                metric: "drive_temperature_c".into(),
                value: format!("{c:.1} C"),
                value_number: Some(c),
                unit: "celsius".into(),
                scope: "system".into(),
                subject: Some(d.device.clone()),
                detail: "Device temperature when the interface reports it.".into(),
            });
        }
        if let Some(p) = d.percentage_used {
            out.push(Measurement {
                metric: "nvme_percentage_used".into(),
                value: format!("{p}%"),
                value_number: Some(p as f64),
                unit: "percent".into(),
                scope: "system".into(),
                subject: Some(d.device.clone()),
                detail: "NVMe percentage of endurance used.".into(),
            });
        }
    }
    if let Some(b) = &data.battery {
        if let Some(h) = b.health_percent {
            out.push(Measurement {
                metric: "battery_health_percent".into(),
                value: format!("{h:.1}%"),
                value_number: Some(h),
                unit: "percent".into(),
                scope: "system".into(),
                subject: Some("battery".into()),
                detail: "Full-charge capacity as a percentage of design capacity.".into(),
            });
        }
        if let Some(p) = b.percent {
            out.push(Measurement {
                metric: "battery_percent".into(),
                value: format!("{p}%"),
                value_number: Some(p as f64),
                unit: "percent".into(),
                scope: "system".into(),
                subject: Some("battery".into()),
                detail: "Current charge level.".into(),
            });
        }
    }
    for w in &data.wifi {
        if let Some(s) = w.signal_percent {
            out.push(Measurement {
                metric: "wifi_signal_percent".into(),
                value: format!("{s}%"),
                value_number: Some(s as f64),
                unit: "percent".into(),
                scope: "system".into(),
                subject: Some(w.description.clone()),
                detail: "OS-reported Wi-Fi signal quality.".into(),
            });
        }
    }
    out
}

fn percent_free(d: &crate::models::SystemDriveEvidence) -> f64 {
    if d.total_bytes > 0 {
        d.free_bytes as f64 / d.total_bytes as f64 * 100.0
    } else {
        100.0
    }
}

/// Threshold rules over the machine-wide evidence: each flag becomes both a
/// signal and a ranked finding.
#[allow(clippy::too_many_lines)]
fn system_signals_and_findings(
    data: &SystemDiagnosticData,
    health: &HealthConfig,
    diagnostics: &DiagnosticsConfig,
) -> (Vec<DiagnosticSignal>, Vec<RankedFinding>) {
    let mut signals = Vec::new();
    let mut findings = Vec::new();

    for d in &data.drives {
        if d.free_bytes > health.low_disk_free_bytes {
            continue;
        }
        let free_pct = percent_free(d);
        let score = storage_score(free_pct);
        let (severity, confidence) = score_bands(score);
        signals.push(DiagnosticSignal {
            kind: SystemSignalKind::StoragePressure.as_str().into(),
            label: SystemSignalKind::StoragePressure.label().into(),
            severity: SystemSignalKind::StoragePressure.severity().into(),
            evidence: vec![EvidencePoint {
                metric: "drive_free_percent".into(),
                value: format!("{free_pct:.1}% free"),
                detail: format!(
                    "threshold: <= {:.0} GB free",
                    health.low_disk_free_bytes as f64 / 1e9
                ),
            }],
        });
        findings.push(RankedFinding {
            rank: 0,
            title: format!("{} storage pressure", capitalize(severity)),
            category: "storage".into(),
            severity: severity.into(),
            confidence: confidence.into(),
            score,
            subject: d.subject.clone(),
            evidence: vec![
                EvidencePoint {
                    metric: "drive_free_percent".into(),
                    value: format!("{free_pct:.1}%"),
                    detail: "of total drive capacity".into(),
                },
                EvidencePoint {
                    metric: "drive_free_bytes".into(),
                    value: format!("{:.1} GB", d.free_bytes as f64 / 1e9),
                    detail: "free space remaining".into(),
                },
            ],
            detail: format!(
                "{} has {:.1} GB free of {:.1} GB ({free_pct:.1}%), below the {:.0} GB low-space threshold.",
                d.subject,
                d.free_bytes as f64 / 1e9,
                d.total_bytes as f64 / 1e9,
                health.low_disk_free_bytes as f64 / 1e9
            ),
        });
    }

    if let Some(load) = data.memory_load_percent {
        if load >= health.high_memory_load_percent {
            let score = memory_pressure_score(load, health.high_memory_load_percent);
            let (severity, confidence) = score_bands(score);
            let available = data.memory_available_bytes.unwrap_or(0);
            signals.push(DiagnosticSignal {
                kind: SystemSignalKind::MemoryPressure.as_str().into(),
                label: SystemSignalKind::MemoryPressure.label().into(),
                severity: SystemSignalKind::MemoryPressure.severity().into(),
                evidence: vec![EvidencePoint {
                    metric: "memory_load_percent".into(),
                    value: format!("{load:.1}%"),
                    detail: format!("threshold: >= {:.0}% load", health.high_memory_load_percent),
                }],
            });
            findings.push(RankedFinding {
                rank: 0,
                title: "System memory pressure".into(),
                category: "memory_pressure".into(),
                severity: severity.into(),
                confidence: confidence.into(),
                score,
                subject: "memory".into(),
                evidence: vec![
                    EvidencePoint {
                        metric: "memory_load_percent".into(),
                        value: format!("{load:.1}%"),
                        detail: "of physical RAM".into(),
                    },
                    EvidencePoint {
                        metric: "memory_available_bytes".into(),
                        value: format!("{:.1} GB", available as f64 / 1e9),
                        detail: "available physical memory".into(),
                    },
                ],
                detail: format!(
                    "System memory load is {load:.1}%, above the {:.0}% pressure threshold; {:.1} GB of {:.1} GB is available.",
                    health.high_memory_load_percent,
                    available as f64 / 1e9,
                    data.memory_total_bytes.unwrap_or(0) as f64 / 1e9
                ),
            });
        }
    }

    for g in &data.app_groups {
        let classification = crate::diagnostics::health::classify_application_group(
            g.cpu_percent,
            g.working_set_bytes,
            health,
        );
        if classification.high_cpu {
            let cpu = g.cpu_percent.unwrap_or(0.0);
            let score = app_cpu_score(cpu);
            let (severity, confidence) = score_bands(score);
            signals.push(DiagnosticSignal {
                kind: SystemSignalKind::AppHighCpu.as_str().into(),
                label: SystemSignalKind::AppHighCpu.label().into(),
                severity: SystemSignalKind::AppHighCpu.severity().into(),
                evidence: vec![EvidencePoint {
                    metric: "cpu_percent".into(),
                    value: format!("{cpu:.1}% of system CPU capacity"),
                    detail: format!(
                        "threshold: >= {:.0}% of system CPU capacity",
                        health.high_cpu_percent
                    ),
                }],
            });
            findings.push(RankedFinding {
                rank: 0,
                title: format!("{} CPU pressure", g.display_name),
                category: "app_cpu".into(),
                severity: severity.into(),
                confidence: confidence.into(),
                score,
                subject: g.display_name.clone(),
                evidence: vec![EvidencePoint {
                    metric: "cpu_percent".into(),
                    value: format!("{cpu:.1}% of system CPU capacity"),
                    detail: "100% = all logical processors fully busy".into(),
                }],
                detail: format!(
                    "{} used {cpu:.1}% of system CPU capacity across {} processes, above the {:.0}% threshold.",
                    g.display_name,
                    g.process_count,
                    health.high_cpu_percent
                ),
            });
        }
        if classification.high_memory {
            let score = app_memory_score(
                g.working_set_bytes,
                data.memory_total_bytes,
                health.high_memory_bytes,
            );
            let (severity, confidence) = score_bands(score);
            signals.push(DiagnosticSignal {
                kind: SystemSignalKind::AppHighMemory.as_str().into(),
                label: SystemSignalKind::AppHighMemory.label().into(),
                severity: SystemSignalKind::AppHighMemory.severity().into(),
                evidence: vec![EvidencePoint {
                    metric: "working_set_bytes".into(),
                    value: format!("{:.1} GB", g.working_set_bytes as f64 / 1e9),
                    detail: format!(
                        "threshold: >= {:.0} GB total working set",
                        health.high_memory_bytes as f64 / 1e9
                    ),
                }],
            });
            findings.push(RankedFinding {
                rank: 0,
                title: format!("{} memory pressure", g.display_name),
                category: "app_memory".into(),
                severity: severity.into(),
                confidence: confidence.into(),
                score,
                subject: g.display_name.clone(),
                evidence: vec![EvidencePoint {
                    metric: "working_set_bytes".into(),
                    value: format!("{:.1} GB", g.working_set_bytes as f64 / 1e9),
                    detail: "total working set across all processes of this executable".into(),
                }],
                detail: format!(
                    "{} holds {:.1} GB of working set across {} processes, above the {:.0} GB threshold.",
                    g.display_name,
                    g.working_set_bytes as f64 / 1e9,
                    g.process_count,
                    health.high_memory_bytes as f64 / 1e9
                ),
            });
        }
    }

    if let Some(rate) = data.memory_growth_bytes_per_second {
        if rate > 0 && (rate as u64) >= diagnostics.system_memory_growth_bytes_per_second {
            let score = memory_growth_score(
                rate as f64,
                diagnostics.system_memory_growth_bytes_per_second as f64,
            );
            let (severity, confidence) = score_bands(score);
            signals.push(DiagnosticSignal {
                kind: SystemSignalKind::MemoryGrowth.as_str().into(),
                label: SystemSignalKind::MemoryGrowth.label().into(),
                severity: SystemSignalKind::MemoryGrowth.severity().into(),
                evidence: vec![EvidencePoint {
                    metric: "memory_growth_bytes_per_second".into(),
                    value: format!("{:.1} MB/s", rate as f64 / (1024.0 * 1024.0)),
                    detail: format!(
                        "threshold: >= {:.0} MB/s over the sampling window",
                        diagnostics.system_memory_growth_bytes_per_second as f64
                            / (1024.0 * 1024.0)
                    ),
                }],
            });
            findings.push(RankedFinding {
                rank: 0,
                title: "Runaway memory growth".into(),
                category: "memory_growth".into(),
                severity: severity.into(),
                confidence: confidence.into(),
                score,
                subject: "system memory".into(),
                evidence: vec![EvidencePoint {
                    metric: "memory_growth_bytes_per_second".into(),
                    value: format!("{:.1} MB/s", rate as f64 / (1024.0 * 1024.0)),
                    detail: "available memory decreasing at this rate".into(),
                }],
                detail: format!(
                    "System available memory decreased at {:.1} MB/s across the sampling window, above the {:.0} MB/s runaway threshold.",
                    rate as f64 / (1024.0 * 1024.0),
                    diagnostics.system_memory_growth_bytes_per_second as f64 / (1024.0 * 1024.0)
                ),
            });
        }
    }

    if let Some(t) = &data.thermal {
        let pressure_score = thermal_pressure_score(&t.cpu_thermal_pressure);
        if pressure_score > 0 {
            let (severity, confidence) = score_bands(pressure_score);
            signals.push(DiagnosticSignal {
                kind: SystemSignalKind::CpuThermalPressure.as_str().into(),
                label: SystemSignalKind::CpuThermalPressure.label().into(),
                severity: SystemSignalKind::CpuThermalPressure.severity().into(),
                evidence: vec![EvidencePoint {
                    metric: "cpu_thermal_pressure".into(),
                    value: t.cpu_thermal_pressure.to_string(),
                    detail: format!(
                        "threshold: >= {:.0} C package temperature",
                        diagnostics.high_cpu_temperature_c
                    ),
                }],
            });
            findings.push(RankedFinding {
                rank: 0,
                title: "CPU thermal pressure".into(),
                category: "thermal".into(),
                severity: severity.into(),
                confidence: confidence.into(),
                score: pressure_score,
                subject: "cpu_package".into(),
                evidence: vec![
                    EvidencePoint {
                        metric: "cpu_thermal_pressure".into(),
                        value: t.cpu_thermal_pressure.clone(),
                        detail: "interpreted from measured temperatures".into(),
                    },
                    EvidencePoint {
                        metric: "cpu_temperature_c".into(),
                        value: t
                            .cpu_temperature_c
                            .map(|c| format!("{c:.1} C"))
                            .unwrap_or_else(|| "unavailable".into()),
                        detail: "highest readable CPU temperature".into(),
                    },
                ],
                detail: format!(
                    "CPU thermal pressure is '{}'; the highest readable CPU temperature was {}. Sustained heat can trigger frequency reduction.",
                    t.cpu_thermal_pressure,
                    t.cpu_temperature_c
                        .map(|c| format!("{c:.1} C"))
                        .unwrap_or_else(|| "unavailable".into())
                ),
            });
        }

        let freq_score = frequency_reduction_score(&t.cpu_throttling, t.cpu_frequency_reduced);
        if freq_score > 0 {
            let (severity, confidence) = score_bands(freq_score);
            let reduced_label = t
                .cpu_frequency_reduced
                .map(|r| r.to_string())
                .unwrap_or_else(|| "unknown".into());
            signals.push(DiagnosticSignal {
                kind: SystemSignalKind::CpuFrequencyReduced.as_str().into(),
                label: SystemSignalKind::CpuFrequencyReduced.label().into(),
                severity: SystemSignalKind::CpuFrequencyReduced.severity().into(),
                evidence: vec![
                    EvidencePoint {
                        metric: "cpu_throttling".into(),
                        value: t.cpu_throttling.clone(),
                        detail: "interpreted from thermal state".into(),
                    },
                    EvidencePoint {
                        metric: "cpu_frequency_reduced".into(),
                        value: reduced_label.clone(),
                        detail: "current clock below base clock".into(),
                    },
                ],
            });
            findings.push(RankedFinding {
                rank: 0,
                title: "CPU frequency reduced".into(),
                category: "frequency".into(),
                severity: severity.into(),
                confidence: confidence.into(),
                score: freq_score,
                subject: "cpu_package".into(),
                evidence: vec![EvidencePoint {
                    metric: "cpu_frequency_reduced".into(),
                    value: reduced_label.clone(),
                    detail: "current/base clock ratio".into(),
                }],
                detail: format!(
                    "CPU throttling is '{}' with frequency_reduced={reduced_label}; the CPU is likely running below its base clock.",
                    t.cpu_throttling
                ),
            });
        }
    }

    for d in &data.storage_health {
        let score = storage_health_score(
            d.health_status.as_deref(),
            d.percentage_used,
            diagnostics.storage_used_warning_percent,
        );
        if score == 0 {
            continue;
        }
        let (severity, confidence) = score_bands(score);
        let status = d.health_status.clone().unwrap_or_else(|| "unknown".into());
        signals.push(DiagnosticSignal {
            kind: SystemSignalKind::StorageHealth.as_str().into(),
            label: SystemSignalKind::StorageHealth.label().into(),
            severity: SystemSignalKind::StorageHealth.severity().into(),
            evidence: vec![EvidencePoint {
                metric: "drive_health_status".into(),
                value: status.clone(),
                detail: format!(
                    "threshold: warning at >= {:.0}% used",
                    diagnostics.storage_used_warning_percent
                ),
            }],
        });
        findings.push(RankedFinding {
            rank: 0,
            title: format!("{} health concern", d.device),
            category: "storage_health".into(),
            severity: severity.into(),
            confidence: confidence.into(),
            score,
            subject: d.device.clone(),
            evidence: vec![
                EvidencePoint {
                    metric: "drive_health_status".into(),
                    value: status,
                    detail: "reported S.M.A.R.T. status".into(),
                },
                EvidencePoint {
                    metric: "drive_temperature_c".into(),
                    value: d
                        .temperature_c
                        .map(|c| format!("{c:.1} C"))
                        .unwrap_or_else(|| "unavailable".into()),
                    detail: "device temperature".into(),
                },
            ],
            detail: format!(
                "{} ({} interface) reports health '{}'{}.",
                d.device,
                d.interface,
                d.health_status.as_deref().unwrap_or("unknown"),
                d.percentage_used
                    .map(|p| format!(" with {p}% of NVMe endurance used"))
                    .unwrap_or_default()
            ),
        });
    }

    if let Some(b) = &data.battery {
        if let Some(h) = b.health_percent {
            let score = battery_health_score(h, diagnostics.low_battery_health_percent);
            if score > 0 {
                let (severity, confidence) = score_bands(score);
                signals.push(DiagnosticSignal {
                    kind: SystemSignalKind::BatteryDegraded.as_str().into(),
                    label: SystemSignalKind::BatteryDegraded.label().into(),
                    severity: SystemSignalKind::BatteryDegraded.severity().into(),
                    evidence: vec![EvidencePoint {
                        metric: "battery_health_percent".into(),
                        value: format!("{h:.1}%"),
                        detail: format!(
                            "threshold: <= {:.0}% of design capacity",
                            diagnostics.low_battery_health_percent
                        ),
                    }],
                });
                findings.push(RankedFinding {
                    rank: 0,
                    title: "Battery degraded".into(),
                    category: "battery".into(),
                    severity: severity.into(),
                    confidence: confidence.into(),
                    score,
                    subject: "battery".into(),
                    evidence: vec![
                        EvidencePoint {
                            metric: "battery_health_percent".into(),
                            value: format!("{h:.1}%"),
                            detail: "full-charge capacity vs design capacity".into(),
                        },
                        EvidencePoint {
                            metric: "battery_percent".into(),
                            value: b.percent.map(|p| format!("{p}%")).unwrap_or_else(|| "unknown".into()),
                            detail: "current charge level".into(),
                        },
                    ],
                    detail: format!(
                        "Battery health is {h:.1}% of design capacity, at or below the {:.0}% degraded threshold.",
                        diagnostics.low_battery_health_percent
                    ),
                });
            }
        }
    }

    for w in &data.wifi {
        if let Some(signal) = w.signal_percent {
            let score = wifi_signal_score(signal, diagnostics.weak_wifi_signal_percent);
            if score == 0 {
                continue;
            }
            let (severity, confidence) = score_bands(score);
            signals.push(DiagnosticSignal {
                kind: SystemSignalKind::WifiWeakSignal.as_str().into(),
                label: SystemSignalKind::WifiWeakSignal.label().into(),
                severity: SystemSignalKind::WifiWeakSignal.severity().into(),
                evidence: vec![EvidencePoint {
                    metric: "wifi_signal_percent".into(),
                    value: format!("{signal}%"),
                    detail: format!(
                        "threshold: <= {:.0}% signal",
                        diagnostics.weak_wifi_signal_percent
                    ),
                }],
            });
            findings.push(RankedFinding {
                rank: 0,
                title: format!("Weak Wi-Fi signal on {}", w.description),
                category: "wifi".into(),
                severity: severity.into(),
                confidence: confidence.into(),
                score,
                subject: w.description.clone(),
                evidence: vec![EvidencePoint {
                    metric: "wifi_signal_percent".into(),
                    value: format!("{signal}%"),
                    detail: "OS-reported signal quality".into(),
                }],
                detail: format!(
                    "Wi-Fi adapter '{}' reports {signal}% signal, at or below the {:.0}% weak-signal threshold.",
                    w.description, diagnostics.weak_wifi_signal_percent
                ),
            });
        }
    }

    (signals, findings)
}

/// The "no evidence of ..." list: dimensions that were actually measured and
/// stayed below every threshold.
fn checked_clean(data: &SystemDiagnosticData, signals: &[DiagnosticSignal]) -> Vec<String> {
    let kinds: Vec<&str> = signals.iter().map(|s| s.kind.as_str()).collect();
    let mut clean = Vec::new();
    if !data.drives.is_empty() && !kinds.contains(&"storage_pressure") {
        clean.push("storage pressure".to_string());
    }
    if data.memory_load_percent.is_some() && !kinds.contains(&"memory_pressure") {
        clean.push("system memory pressure".to_string());
    }
    if !data.app_groups.is_empty()
        && !kinds.contains(&"app_high_cpu")
        && !kinds.contains(&"app_high_memory")
    {
        clean.push("application resource pressure".to_string());
    }
    if data.memory_growth_bytes_per_second.is_some() && !kinds.contains(&"memory_growth") {
        clean.push("runaway memory growth".to_string());
    }
    if data.thermal.is_some()
        && !kinds.contains(&"cpu_thermal_pressure")
        && !kinds.contains(&"cpu_frequency_reduced")
    {
        clean.push("CPU thermal state".to_string());
    }
    if !data.storage_health.is_empty() && !kinds.contains(&"storage_health") {
        clean.push("storage health".to_string());
    }
    if data
        .battery
        .as_ref()
        .and_then(|b| b.health_percent)
        .is_some()
        && !kinds.contains(&"battery_degraded")
    {
        clean.push("battery health".to_string());
    }
    if !data.wifi.is_empty() && !kinds.contains(&"wifi_weak_signal") {
        clean.push("Wi-Fi signal strength".to_string());
    }
    clean
}

/// Represent a ranked finding as a possible cause, keeping the report's
/// evidence-first shape consistent with tab reports.
fn finding_as_possible_cause(f: &RankedFinding) -> PossibleCause {
    let signal_kind = match f.category.as_str() {
        "storage" => "storage_pressure",
        "memory_pressure" => "memory_pressure",
        "app_cpu" => "app_high_cpu",
        "app_memory" => "app_high_memory",
        "thermal" => "cpu_thermal_pressure",
        "frequency" => "cpu_frequency_reduced",
        "storage_health" => "storage_health",
        "battery" => "battery_degraded",
        "wifi" => "wifi_weak_signal",
        _ => "memory_growth",
    };
    PossibleCause {
        hypothesis: f.title.clone(),
        supporting_signals: vec![signal_kind.to_string()],
        confidence: f.confidence.clone(),
        confidence_value: (f.score as f64 / 100.0).clamp(0.0, 1.0),
        reasoning: format!(
            "{} Confidence is derived from the deterministic severity score ({}/100); this is a heuristic ranking of measured evidence, not a verified root cause.",
            f.detail, f.score
        ),
    }
}

fn capitalize(s: &str) -> String {
    let mut chars = s.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
        None => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_data() -> SystemDiagnosticData {
        SystemDiagnosticData {
            memory_load_percent: Some(92.0),
            memory_available_bytes: Some(1_280_000_000),
            memory_total_bytes: Some(16_000_000_000),
            memory_growth_bytes_per_second: Some(60 * 1024 * 1024),
            drives: vec![crate::models::SystemDriveEvidence {
                subject: "C:".into(),
                free_bytes: 5_000_000_000,
                total_bytes: 500_000_000_000,
            }],
            app_groups: vec![
                crate::models::SystemAppEvidence {
                    name: "chrome".into(),
                    display_name: "Google Chrome".into(),
                    process_count: 12,
                    working_set_bytes: 4_600_000_000,
                    cpu_percent: Some(57.0),
                },
                crate::models::SystemAppEvidence {
                    name: "node".into(),
                    display_name: "Node.js".into(),
                    process_count: 1,
                    working_set_bytes: 2_100_000_000,
                    cpu_percent: Some(2.0),
                },
            ],
            ..SystemDiagnosticData::default()
        }
    }

    #[test]
    fn unhealthy_machine_produces_ranked_findings() {
        let diagnosis = analyze_system(
            &sample_data(),
            &DiagnosticsConfig::default(),
            &HealthConfig::default(),
        );
        assert_eq!(diagnosis.report.status, "signals_detected");
        assert!(!diagnosis.findings.is_empty());
        // Storage (1% free => 100) ranks above chrome CPU (57) above node memory.
        assert_eq!(diagnosis.findings[0].category, "storage");
        assert_eq!(diagnosis.findings[0].score, 100);
        assert_eq!(diagnosis.findings[0].rank, 1);
        let ranks: Vec<usize> = diagnosis.findings.iter().map(|f| f.rank).collect();
        let mut sorted = ranks.clone();
        sorted.sort();
        assert_eq!(ranks, sorted);
        // Evidence references measurements present in the report.
        for f in &diagnosis.findings {
            for e in &f.evidence {
                assert!(
                    diagnosis
                        .report
                        .measurements
                        .iter()
                        .any(|m| m.metric == e.metric),
                    "finding evidence '{}' has no matching measurement",
                    e.metric
                );
            }
        }
    }

    #[test]
    fn checked_clean_lists_only_measured_clean_dimensions() {
        let diagnosis = analyze_system(
            &sample_data(),
            &DiagnosticsConfig::default(),
            &HealthConfig::default(),
        );
        // Runaway growth (60 MB/s) and storage fired; memory pressure (92%)
        // fired too. So the clean list must not contain those.
        assert!(!diagnosis
            .checked_clean
            .contains(&"runaway memory growth".to_string()));
        assert!(!diagnosis
            .checked_clean
            .contains(&"storage pressure".to_string()));
        assert!(!diagnosis
            .checked_clean
            .contains(&"system memory pressure".to_string()));
    }

    #[test]
    fn quiet_machine_reports_negative_status_and_checked_clean() {
        let data = SystemDiagnosticData {
            memory_load_percent: Some(30.0),
            memory_available_bytes: Some(11_200_000_000),
            memory_total_bytes: Some(16_000_000_000),
            memory_growth_bytes_per_second: Some(1024 * 1024),
            drives: vec![crate::models::SystemDriveEvidence {
                subject: "C:".into(),
                free_bytes: 200_000_000_000,
                total_bytes: 500_000_000_000,
            }],
            app_groups: vec![crate::models::SystemAppEvidence {
                name: "node".into(),
                display_name: "Node.js".into(),
                process_count: 1,
                working_set_bytes: 100_000_000,
                cpu_percent: Some(1.0),
            }],
            ..SystemDiagnosticData::default()
        };
        let diagnosis = analyze_system(
            &data,
            &DiagnosticsConfig::default(),
            &HealthConfig::default(),
        );
        assert_eq!(diagnosis.report.status, "no_supported_signal_detected");
        assert!(diagnosis.findings.is_empty());
        assert!(diagnosis
            .report
            .agent_guidance
            .contains("Do not infer a cause"));
        assert_eq!(
            diagnosis.checked_clean,
            vec![
                "storage pressure".to_string(),
                "system memory pressure".to_string(),
                "application resource pressure".to_string(),
                "runaway memory growth".to_string(),
            ]
        );
    }

    #[test]
    fn missing_evidence_marks_report_limited() {
        let data = SystemDiagnosticData {
            memory_load_percent: None,
            ..SystemDiagnosticData::default()
        };
        let diagnosis = analyze_system(
            &data,
            &DiagnosticsConfig::default(),
            &HealthConfig::default(),
        );
        assert_eq!(diagnosis.report.evidence_completeness, "limited");
        // A dimension that was never measured must not appear as checked clean.
        assert!(!diagnosis
            .checked_clean
            .contains(&"runaway memory growth".to_string()));
        assert!(!diagnosis
            .checked_clean
            .contains(&"system memory pressure".to_string()));
    }

    #[test]
    fn below_threshold_growth_does_not_fire() {
        let data = SystemDiagnosticData {
            memory_load_percent: Some(50.0),
            memory_growth_bytes_per_second: Some(1024 * 1024),
            app_groups: vec![crate::models::SystemAppEvidence {
                name: "node".into(),
                display_name: "Node.js".into(),
                process_count: 1,
                working_set_bytes: 100_000_000,
                cpu_percent: None,
            }],
            ..SystemDiagnosticData::default()
        };
        let diagnosis = analyze_system(
            &data,
            &DiagnosticsConfig::default(),
            &HealthConfig::default(),
        );
        assert!(!diagnosis
            .report
            .signals
            .iter()
            .any(|s| s.kind == "memory_growth"));
        assert!(diagnosis
            .checked_clean
            .contains(&"runaway memory growth".to_string()));
    }

    #[test]
    fn hardware_evidence_produces_cross_domain_findings() {
        let data = SystemDiagnosticData {
            thermal: Some(crate::models::SystemThermalEvidence {
                cpu_thermal_pressure: "high".into(),
                cpu_throttling: "likely".into(),
                cpu_frequency_reduced: Some(true),
                cpu_temperature_c: Some(96.0),
                gpu_thermal_pressure: "low".into(),
            }),
            storage_health: vec![crate::models::SystemStorageHealthEvidence {
                device: "PhysicalDrive0".into(),
                interface: "nvme".into(),
                health_status: Some("warning".into()),
                temperature_c: Some(58.0),
                percentage_used: Some(85),
            }],
            battery: Some(crate::models::SystemBatteryEvidence {
                present: true,
                percent: Some(80),
                ac_online: Some(true),
                charging: Some(true),
                battery_state: Some("charging".into()),
                health_percent: Some(40.0),
            }),
            wifi: vec![crate::models::SystemWifiEvidence {
                description: "Intel Wi-Fi 6E".into(),
                state: "connected".into(),
                signal_percent: Some(15),
                link_speed_mbps: Some(1200.0),
            }],
            ..SystemDiagnosticData::default()
        };
        let diagnosis = analyze_system(
            &data,
            &DiagnosticsConfig::default(),
            &HealthConfig::default(),
        );
        let kinds: Vec<&str> = diagnosis
            .report
            .signals
            .iter()
            .map(|s| s.kind.as_str())
            .collect();
        assert!(kinds.contains(&"cpu_thermal_pressure"));
        assert!(kinds.contains(&"cpu_frequency_reduced"));
        assert!(kinds.contains(&"storage_health"));
        assert!(kinds.contains(&"battery_degraded"));
        assert!(kinds.contains(&"wifi_weak_signal"));
        // Thermal pressure (90) outranks the frequency reduction (85), and
        // every finding's evidence has a backing measurement.
        assert_eq!(diagnosis.findings[0].category, "thermal");
        for f in &diagnosis.findings {
            for e in &f.evidence {
                assert!(
                    diagnosis
                        .report
                        .measurements
                        .iter()
                        .any(|m| m.metric == e.metric),
                    "finding evidence '{}' has no matching measurement",
                    e.metric
                );
            }
        }
    }

    #[test]
    fn healthy_hardware_evidence_reports_checked_clean() {
        let data = SystemDiagnosticData {
            thermal: Some(crate::models::SystemThermalEvidence {
                cpu_thermal_pressure: "low".into(),
                cpu_throttling: "not_observed".into(),
                cpu_frequency_reduced: Some(false),
                cpu_temperature_c: Some(52.0),
                gpu_thermal_pressure: "low".into(),
            }),
            storage_health: vec![crate::models::SystemStorageHealthEvidence {
                device: "PhysicalDrive0".into(),
                interface: "nvme".into(),
                health_status: Some("healthy".into()),
                temperature_c: Some(42.0),
                percentage_used: Some(30),
            }],
            battery: Some(crate::models::SystemBatteryEvidence {
                present: true,
                percent: Some(90),
                ac_online: Some(true),
                charging: Some(true),
                battery_state: Some("charging".into()),
                health_percent: Some(95.0),
            }),
            wifi: vec![crate::models::SystemWifiEvidence {
                description: "Intel Wi-Fi 6E".into(),
                state: "connected".into(),
                signal_percent: Some(85),
                link_speed_mbps: Some(1200.0),
            }],
            ..SystemDiagnosticData::default()
        };
        let diagnosis = analyze_system(
            &data,
            &DiagnosticsConfig::default(),
            &HealthConfig::default(),
        );
        assert!(diagnosis.findings.is_empty());
        assert!(diagnosis
            .checked_clean
            .contains(&"CPU thermal state".to_string()));
        assert!(diagnosis
            .checked_clean
            .contains(&"storage health".to_string()));
        assert!(diagnosis
            .checked_clean
            .contains(&"battery health".to_string()));
        assert!(diagnosis
            .checked_clean
            .contains(&"Wi-Fi signal strength".to_string()));
    }
}
