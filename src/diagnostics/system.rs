//! Machine-wide diagnosis (§77): system evidence → ranked findings.
//!
//! `analyze_system` takes machine-wide evidence (memory, drives, application
//! groups, memory growth), applies the same evidence-first discipline as tab
//! diagnosis — measurements are reported separately from signals — and adds a
//! deterministic ranking of findings plus a "checked clean" list. It is a
//! pure, testable function: no Windows calls, no LLM.

use crate::config::{DiagnosticsConfig, HealthConfig};
use crate::diagnostics::findings::{
    app_cpu_score, app_memory_score, memory_growth_score, memory_pressure_score, score_bands,
    storage_score,
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
}

impl SystemSignalKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::StoragePressure => "storage_pressure",
            Self::MemoryPressure => "memory_pressure",
            Self::AppHighCpu => "app_high_cpu",
            Self::AppHighMemory => "app_high_memory",
            Self::MemoryGrowth => "memory_growth",
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::StoragePressure => "Storage pressure",
            Self::MemoryPressure => "System memory pressure",
            Self::AppHighCpu => "High application CPU usage",
            Self::AppHighMemory => "High application memory usage",
            Self::MemoryGrowth => "Runaway memory growth",
        }
    }

    fn severity(self) -> &'static str {
        match self {
            Self::StoragePressure => "high",
            Self::MemoryPressure => "high",
            Self::AppHighCpu => "medium",
            Self::AppHighMemory => "medium",
            Self::MemoryGrowth => "high",
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
            "Memory growth is sampled over a short window (~1 s) and may miss slower trends; network and service stability are not part of this diagnosis.".to_string(),
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
}
