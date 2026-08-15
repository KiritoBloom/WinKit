//! Deterministic diagnostic engine (§34, §73, §74, §77).
//!
//! The engine converts measured evidence into signals via explicit
//! thresholds, then correlates signals into evidence-backed possible causes
//! with confidence levels. It is a pure, testable state machine — no LLM,
//! no randomness, no fabricated claims. Reports are evidence-first:
//! `measurements` (facts), `signals` (interpretations), `possible_causes`
//! (hypotheses) are separate fields.

pub mod correlation;
pub mod findings;
pub mod health;
pub mod scoring;
pub mod system;

pub use crate::models::TabDiagnosticData;
pub use correlation::{CorrelationRule, POSSIBLE_CAUSE_RULES};
pub use scoring::{SignalKind, SIGNAL_RULES};

use crate::config::DiagnosticsConfig;
use crate::models::{DiagnosticReport, EvidencePoint, Measurement};

/// The engine: thresholds (scoring) + correlation rules.
#[derive(Debug, Clone, Default)]
pub struct DiagnosticsEngine {
    pub config: DiagnosticsConfig,
}

impl DiagnosticsEngine {
    pub fn with_config(config: DiagnosticsConfig) -> Self {
        Self { config }
    }

    pub fn with_defaults() -> Self {
        Self::default()
    }

    /// Analyze tab evidence into a full report.
    pub fn analyze_tab(&self, data: &TabDiagnosticData) -> DiagnosticReport {
        let signals = scoring::compute_signals(data, &self.config);
        let correlations = correlation::compute_correlations(&signals);
        let possible_causes = correlation::compute_possible_causes(&signals, &correlations);
        let status = if signals.is_empty() {
            "no_supported_signal_detected".to_string()
        } else {
            "signals_detected".to_string()
        };
        let evidence_completeness =
            if data.js_heap_used_bytes.is_none() || data.cpu_percent.is_none() {
                "limited".to_string()
            } else {
                "full".to_string()
            };
        let agent_guidance = if signals.is_empty() {
            "No supported evidence was found. Do not infer a cause from resource usage alone."
                .to_string()
        } else {
            "Signals and possible causes are deterministic heuristics over measured evidence; treat them as hypotheses, not verified root causes."
                .to_string()
        };
        DiagnosticReport {
            target: format!(
                "chrome tab (heap={:?}, cpu={:?})",
                data.js_heap_used_bytes, data.cpu_percent
            ),
            generated_at: crate::utils::time::format_rfc3339(std::time::SystemTime::now()),
            status,
            evidence_completeness,
            measurements: tab_measurements(data),
            signals,
            correlations,
            possible_causes,
            limitations: self.limitations(),
            agent_guidance,
        }
    }

    /// Statements every report should carry so agents do not over-read it.
    pub fn limitations(&self) -> Vec<String> {
        vec![
            "Thresholds are heuristic defaults from the diagnostics configuration; they are not guarantees of a problem.".to_string(),
            "CPU percentages are relative to total system CPU capacity across all logical processors; 100% means every core is fully busy, and a single busy core on an N-core machine reports 100/N percent.".to_string(),
            "Network and runtime signals are sampled over a short observation window and may miss activity outside it.".to_string(),
        ]
    }
}

/// Build the report's raw measurement list from tab evidence. Only values
/// that were actually measured appear; zero-valued counters are omitted
/// because the raw sections already carry exact zeros and the absence of a
/// signal is the honest negative statement.
pub fn tab_measurements(data: &TabDiagnosticData) -> Vec<Measurement> {
    let mut out = Vec::new();
    if let Some(v) = data.cpu_percent {
        out.push(Measurement {
            metric: "cpu_percent".into(),
            value: format!("{v:.1}% of system CPU capacity"),
            value_number: Some(v),
            unit: "percent_of_system_cpu_capacity".into(),
            scope: "browser_aggregate".into(),
            subject: None,
            detail: "Aggregate across all chrome.exe processes; 100% = all logical processors fully busy.".into(),
        });
    }
    if let Some(v) = data.js_heap_used_bytes {
        out.push(Measurement {
            metric: "js_heap_used_bytes".into(),
            value: format!("{} MB", v / (1024 * 1024)),
            value_number: Some(v as f64),
            unit: "bytes".into(),
            scope: "tab".into(),
            subject: None,
            detail: "V8 heap usage reported by performance.memory.".into(),
        });
    }
    if let Some(v) = data.dom_nodes {
        out.push(Measurement {
            metric: "dom_nodes".into(),
            value: v.to_string(),
            value_number: Some(v as f64),
            unit: "count".into(),
            scope: "tab".into(),
            subject: None,
            detail: "DOM node count reported by performance.memory.".into(),
        });
    }
    if let Some(v) = data.heap_growth_bytes_per_second {
        out.push(Measurement {
            metric: "heap_growth_bytes_per_second".into(),
            value: format!("{:.1} MB/s", v as f64 / (1024.0 * 1024.0)),
            value_number: Some(v as f64),
            unit: "bytes_per_second".into(),
            scope: "tab".into(),
            subject: None,
            detail: "Heap growth rate across two samples.".into(),
        });
    }
    if data.heap_growth_sustained {
        out.push(Measurement {
            metric: "heap_growth_sustained".into(),
            value: "true".into(),
            value_number: Some(1.0),
            unit: "boolean".into(),
            scope: "tab".into(),
            subject: None,
            detail:
                "Heap samples trended upward repeatedly across the time series, not a single spike."
                    .into(),
        });
    }
    if data.script_ms > 0.0 {
        out.push(Measurement {
            metric: "script_ms".into(),
            value: format!("{:.0} ms", data.script_ms),
            value_number: Some(data.script_ms),
            unit: "milliseconds".into(),
            scope: "tab".into(),
            subject: None,
            detail: "Main-thread script execution during the observation window.".into(),
        });
    }
    if data.long_task_ms > 0.0 {
        out.push(Measurement {
            metric: "long_task_ms".into(),
            value: format!("{:.0} ms", data.long_task_ms),
            value_number: Some(data.long_task_ms),
            unit: "milliseconds".into(),
            scope: "tab".into(),
            subject: None,
            detail: "Long-task duration during the observation window.".into(),
        });
    }
    if data.total_requests > 0 {
        out.push(Measurement {
            metric: "total_requests".into(),
            value: data.total_requests.to_string(),
            value_number: Some(data.total_requests as f64),
            unit: "count".into(),
            scope: "tab".into(),
            subject: None,
            detail: "Requests observed during the window.".into(),
        });
        if data.failed_requests > 0 {
            out.push(Measurement {
                metric: "failed_requests".into(),
                value: data.failed_requests.to_string(),
                value_number: Some(data.failed_requests as f64),
                unit: "count".into(),
                scope: "tab".into(),
                subject: None,
                detail: "Requests that failed during the window.".into(),
            });
        }
    }
    if let Some(v) = data.avg_response_ms {
        out.push(Measurement {
            metric: "avg_response_ms".into(),
            value: format!("{v:.0} ms"),
            value_number: Some(v),
            unit: "milliseconds".into(),
            scope: "tab".into(),
            subject: None,
            detail: "Average response time during the window.".into(),
        });
    }
    if let Some(v) = data.p95_response_ms {
        out.push(Measurement {
            metric: "p95_response_ms".into(),
            value: format!("{v:.0} ms"),
            value_number: Some(v),
            unit: "milliseconds".into(),
            scope: "tab".into(),
            subject: None,
            detail: "95th percentile response time during the window.".into(),
        });
    }
    if let Some(v) = data.bytes_transferred {
        out.push(Measurement {
            metric: "bytes_transferred".into(),
            value: format!("{} MB", v / (1024 * 1024)),
            value_number: Some(v as f64),
            unit: "bytes".into(),
            scope: "tab".into(),
            subject: None,
            detail: "Encoded data transferred during the window.".into(),
        });
    }
    if data.console_errors > 0 {
        out.push(Measurement {
            metric: "console_errors".into(),
            value: data.console_errors.to_string(),
            value_number: Some(data.console_errors as f64),
            unit: "count".into(),
            scope: "tab".into(),
            subject: None,
            detail: "Console error messages during the window.".into(),
        });
    }
    if data.exceptions > 0 {
        out.push(Measurement {
            metric: "exceptions".into(),
            value: data.exceptions.to_string(),
            value_number: Some(data.exceptions as f64),
            unit: "count".into(),
            scope: "tab".into(),
            subject: None,
            detail: "Uncaught exceptions during the window.".into(),
        });
    }
    out
}

/// Convenience: build a single signal's evidence list.
pub fn evidence(metric: &str, value: String, detail: String) -> Vec<EvidencePoint> {
    vec![EvidencePoint {
        metric: metric.to_string(),
        value,
        detail,
    }]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_data() -> TabDiagnosticData {
        TabDiagnosticData {
            cpu_percent: Some(45.0),
            js_heap_used_bytes: Some(700 * 1024 * 1024),
            heap_growth_bytes_per_second: Some(4 * 1024 * 1024),
            long_task_ms: 2_400.0,
            script_ms: 3_100.0,
            dom_nodes: Some(90_000),
            total_requests: 100,
            failed_requests: 30,
            avg_response_ms: Some(800.0),
            p95_response_ms: Some(2_000.0),
            bytes_transferred: Some(20 * 1024 * 1024),
            console_errors: 8,
            exceptions: 4,
            heap_growth_sustained: true,
        }
    }

    #[test]
    fn heavy_tab_produces_signals_and_causes() {
        let engine = DiagnosticsEngine::with_defaults();
        let report = engine.analyze_tab(&sample_data());
        assert!(!report.signals.is_empty());
        assert!(!report.possible_causes.is_empty());
        let kinds: Vec<&str> = report.signals.iter().map(|s| s.kind.as_str()).collect();
        assert!(kinds.contains(&"high_cpu"));
        assert!(kinds.contains(&"high_memory"));
        assert!(kinds.contains(&"rapid_heap_growth"));
    }

    #[test]
    fn quiet_tab_produces_no_signals() {
        let engine = DiagnosticsEngine::with_defaults();
        let report = engine.analyze_tab(&TabDiagnosticData::default());
        assert!(report.signals.is_empty());
        assert!(report.possible_causes.is_empty());
    }

    #[test]
    fn negative_report_is_explicit_and_guides_the_agent() {
        let engine = DiagnosticsEngine::with_defaults();
        let report = engine.analyze_tab(&TabDiagnosticData::default());
        assert_eq!(report.status, "no_supported_signal_detected");
        assert!(report.agent_guidance.contains("Do not infer a cause"));
    }

    #[test]
    fn missing_core_measurements_mark_evidence_limited() {
        let engine = DiagnosticsEngine::with_defaults();
        let mut data = sample_data();
        data.js_heap_used_bytes = None;
        let report = engine.analyze_tab(&data);
        assert_eq!(report.evidence_completeness, "limited");
        let report_full = engine.analyze_tab(&sample_data());
        assert_eq!(report_full.evidence_completeness, "full");
        assert_eq!(report_full.status, "signals_detected");
    }

    #[test]
    fn causes_reference_existing_signals() {
        let engine = DiagnosticsEngine::with_defaults();
        let report = engine.analyze_tab(&sample_data());
        let signal_kinds: Vec<&str> = report.signals.iter().map(|s| s.kind.as_str()).collect();
        for cause in &report.possible_causes {
            for sig in &cause.supporting_signals {
                assert!(
                    signal_kinds.contains(&sig.as_str()),
                    "cause references signal '{sig}' that was not emitted"
                );
            }
        }
    }

    #[test]
    fn measurements_are_facts_separate_from_signals() {
        let engine = DiagnosticsEngine::with_defaults();
        let report = engine.analyze_tab(&sample_data());
        assert!(!report.measurements.is_empty());
        assert!(report
            .measurements
            .iter()
            .any(|m| m.metric == "cpu_percent" && m.scope == "browser_aggregate"));
        assert!(report
            .measurements
            .iter()
            .any(|m| m.metric == "js_heap_used_bytes" && m.scope == "tab"));
        // Every signal's evidence metric must exist in the measurements list.
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
    }

    #[test]
    fn zero_counters_are_omitted_from_measurements() {
        let data = TabDiagnosticData {
            total_requests: 0,
            failed_requests: 0,
            console_errors: 0,
            exceptions: 0,
            long_task_ms: 0.0,
            script_ms: 0.0,
            ..TabDiagnosticData::default()
        };
        let measurements = tab_measurements(&data);
        assert!(!measurements.iter().any(|m| m.metric == "total_requests"));
        assert!(!measurements.iter().any(|m| m.metric == "console_errors"));
        assert!(!measurements.iter().any(|m| m.metric == "script_ms"));
    }
}
