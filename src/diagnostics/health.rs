//! Shared application-group classification (§8.3).
//!
//! `system_health` and `system_diagnose` must agree on every threshold,
//! status label, CPU basis, and missing-evidence behavior. The single source
//! of truth lives here as a pure function; both tools call it, and a
//! regression test proves they agree.
//!
//! Contract:
//! - CPU percent is relative to total system CPU capacity (100% = all
//!   logical processors fully busy). Missing CPU evidence (`None`) never
//!   counts as high CPU.
//! - Memory is total working set across the group's processes in bytes.
//! - Status labels: `high_cpu`, `high_memory`, `high_cpu_and_memory`,
//!   `normal` — `normal` is also the label for a group whose CPU was never
//!   measured but whose memory is below threshold.

use crate::config::HealthConfig;

/// The classification of one application group.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AppGroupClassification {
    pub high_cpu: bool,
    pub high_memory: bool,
}

impl AppGroupClassification {
    /// The shared status label. Must stay in sync with
    /// `docs/diagnostics.md` and `models::ApplicationGroupInfo.status`.
    pub fn status(self) -> &'static str {
        match (self.high_cpu, self.high_memory) {
            (true, true) => "high_cpu_and_memory",
            (true, false) => "high_cpu",
            (false, true) => "high_memory",
            (false, false) => "normal",
        }
    }
}

/// Classify one application group against the shared thresholds.
///
/// `cpu_percent: None` means the CPU basis could not be sampled; the group
/// is never flagged high-CPU on missing evidence. Working set is compared
/// with `>=` so a group exactly at the threshold is flagged (boundary
/// behavior is part of the shared contract).
pub fn classify_application_group(
    cpu_percent: Option<f64>,
    working_set_bytes: u64,
    cfg: &HealthConfig,
) -> AppGroupClassification {
    let high_cpu = cpu_percent
        .map(|v| v >= cfg.high_cpu_percent)
        .unwrap_or(false);
    let high_memory = working_set_bytes >= cfg.high_memory_bytes;
    AppGroupClassification {
        high_cpu,
        high_memory,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg() -> HealthConfig {
        HealthConfig::default()
    }

    #[test]
    fn boundary_values_follow_the_shared_contract() {
        let c = cfg();
        // Exactly at the CPU threshold counts as high.
        assert!(classify_application_group(Some(c.high_cpu_percent), 0, &c).high_cpu);
        // Just below does not.
        assert!(!classify_application_group(Some(c.high_cpu_percent - 0.01), 0, &c).high_cpu);
        // Exactly at the memory threshold counts as high.
        assert!(classify_application_group(None, c.high_memory_bytes, &c).high_memory);
        assert!(!classify_application_group(None, c.high_memory_bytes - 1, &c).high_memory);
    }

    #[test]
    fn missing_cpu_evidence_never_flags_high_cpu() {
        let c = cfg();
        let classification = classify_application_group(None, 0, &c);
        assert!(!classification.high_cpu);
        assert_eq!(classification.status(), "normal");
    }

    #[test]
    fn status_labels_cover_all_combinations() {
        let c = cfg();
        assert_eq!(
            classify_application_group(Some(99.0), 0, &c).status(),
            "high_cpu"
        );
        assert_eq!(
            classify_application_group(None, u64::MAX, &c).status(),
            "high_memory"
        );
        assert_eq!(
            classify_application_group(Some(99.0), u64::MAX, &c).status(),
            "high_cpu_and_memory"
        );
        assert_eq!(
            classify_application_group(Some(1.0), 0, &c).status(),
            "normal"
        );
    }

    #[test]
    fn system_health_and_system_diagnose_agree_on_group_classification() {
        // Prove the regression contract of §8.3 with real consumer data:
        // both tools must flag the same groups and produce the same labels.
        let health = cfg();
        let groups_data: Vec<(f64, u64)> = vec![
            // group: chrome — high CPU + high memory
            (45.0, 3_500 * 1024 * 1024),
            // group: node — high memory only
            (2.0, 2_500 * 1024 * 1024),
            // group: explorer — normal
            (1.0, 100 * 1024 * 1024),
        ];
        let groups: Vec<crate::models::ApplicationGroupInfo> = groups_data
            .iter()
            .enumerate()
            .map(|(i, (cpu, ws))| crate::models::ApplicationGroupInfo {
                name: format!("app{i}"),
                display_name: format!("App {i}"),
                process_count: 1,
                total_working_set_bytes: *ws,
                cpu_percent: Some(*cpu),
                cpu_percent_basis: "system_capacity_all_cores".into(),
                cpu_percent_sample_ms: 300,
                status: "normal".into(),
            })
            .collect();
        let resources = crate::models::ResourceSnapshot {
            cpu_busy_percent: None,
            cpu_busy_percent_basis: "system_capacity_all_cores".into(),
            memory_load_percent: Some(40.0),
            total_memory_bytes: Some(16_000_000_000),
            available_memory_bytes: Some(9_600_000_000),
        };
        let health_report =
            crate::tools::health::build_health_report(groups.clone(), &resources, &[], &health);

        let data = crate::models::SystemDiagnosticData {
            memory_load_percent: Some(40.0),
            memory_available_bytes: Some(9_600_000_000),
            memory_total_bytes: Some(16_000_000_000),
            memory_growth_bytes_per_second: None,
            drives: Vec::new(),
            app_groups: groups
                .iter()
                .map(|g| crate::models::SystemAppEvidence {
                    name: g.name.clone(),
                    display_name: g.display_name.clone(),
                    process_count: g.process_count,
                    working_set_bytes: g.total_working_set_bytes,
                    cpu_percent: g.cpu_percent,
                })
                .collect(),
        };
        let diagnosis = crate::diagnostics::system::analyze_system(
            &data,
            &crate::config::DiagnosticsConfig::default(),
            &health,
        );

        for g in &health_report.applications {
            let sys_equivalent = diagnosis
                .report
                .measurements
                .iter()
                .filter(|m| {
                    m.scope == "application" && m.subject.as_deref() == Some(&g.display_name)
                })
                .any(|_| true);
            assert!(sys_equivalent, "every app group is measured by both tools");
        }
        let hc_statuses: Vec<&str> = health_report
            .applications
            .iter()
            .map(|g| g.status.as_str())
            .collect();
        let sys_cpu_flag: Vec<bool> = groups_data
            .iter()
            .map(|(cpu, _)| *cpu >= health.high_cpu_percent)
            .collect();
        let sys_mem_flag: Vec<bool> = groups_data
            .iter()
            .map(|(_, ws)| *ws >= health.high_memory_bytes)
            .collect();
        let expected: Vec<&str> = sys_cpu_flag
            .iter()
            .zip(&sys_mem_flag)
            .map(|(c, m)| match (c, m) {
                (true, true) => "high_cpu_and_memory",
                (true, false) => "high_cpu",
                (false, true) => "high_memory",
                (false, false) => "normal",
            })
            .collect();
        assert_eq!(hc_statuses, expected);

        let sys_findings: Vec<String> = diagnosis
            .findings
            .iter()
            .filter(|f| matches!(f.category.as_str(), "app_cpu" | "app_memory"))
            .map(|f| f.title.clone())
            .collect();
        assert!(
            sys_findings.iter().any(|t| t.contains("App 0")),
            "the group both tools flag should appear in system_diagnose findings: {sys_findings:?}"
        );
        assert!(
            !sys_findings.iter().any(|t| t.contains("App 2")),
            "a normal group must never appear in system_diagnose findings"
        );
    }
}
