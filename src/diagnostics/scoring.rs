//! Signal computation: thresholds over evidence (§73).
//!
//! Every signal is produced by a deterministic rule with documented
//! thresholds. Threshold defaults live in `DiagnosticsConfig`.

use crate::config::DiagnosticsConfig;
use crate::diagnostics::evidence;
use crate::models::{DiagnosticSignal, EvidencePoint, TabDiagnosticData};

/// Kinds of signals the engine can emit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SignalKind {
    HighCpu,
    HighMemory,
    RapidHeapGrowth,
    SustainedHeapGrowth,
    HighJsActivity,
    ManyLongTasks,
    ManyFailedRequests,
    HighRequestLatency,
    HeavyNetworkActivity,
    RuntimeErrors,
}

impl SignalKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::HighCpu => "high_cpu",
            Self::HighMemory => "high_memory",
            Self::RapidHeapGrowth => "rapid_heap_growth",
            Self::SustainedHeapGrowth => "sustained_heap_growth",
            Self::HighJsActivity => "high_js_activity",
            Self::ManyLongTasks => "many_long_tasks",
            Self::ManyFailedRequests => "many_failed_requests",
            Self::HighRequestLatency => "high_request_latency",
            Self::HeavyNetworkActivity => "heavy_network_activity",
            Self::RuntimeErrors => "runtime_errors",
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::HighCpu => "High CPU activity",
            Self::HighMemory => "High memory usage",
            Self::RapidHeapGrowth => "Rapid heap growth",
            Self::SustainedHeapGrowth => "Sustained heap growth",
            Self::HighJsActivity => "Heavy JavaScript activity",
            Self::ManyLongTasks => "Many long tasks",
            Self::ManyFailedRequests => "Many failed network requests",
            Self::HighRequestLatency => "High request latency",
            Self::HeavyNetworkActivity => "Heavy network activity",
            Self::RuntimeErrors => "Page runtime errors",
        }
    }
}

/// One signal rule: emit the signal when `condition` holds.
pub struct SignalRule {
    pub kind: SignalKind,
    pub severity: &'static str,
    pub condition: fn(&TabDiagnosticData, &DiagnosticsConfig) -> bool,
    pub evidence: fn(&TabDiagnosticData, &DiagnosticsConfig) -> Vec<EvidencePoint>,
}

/// The full, ordered signal rule set.
pub const SIGNAL_RULES: &[SignalRule] = &[
    SignalRule {
        kind: SignalKind::HighCpu,
        severity: "medium",
        condition: |d, c| {
            d.cpu_percent
                .map(|v| v >= c.high_cpu_percent)
                .unwrap_or(false)
        },
        evidence: |d, c| {
            evidence(
                "cpu_percent",
                format!(
                    "{:.1}% of system CPU capacity",
                    d.cpu_percent.unwrap_or(0.0)
                ),
                format!(
                    "threshold: >= {:.0}% of system CPU capacity (100% = all cores busy)",
                    c.high_cpu_percent
                ),
            )
        },
    },
    SignalRule {
        kind: SignalKind::HighMemory,
        severity: "high",
        condition: |d, c| {
            let heap_high = d
                .js_heap_used_bytes
                .map(|v| v >= c.high_heap_bytes)
                .unwrap_or(false);
            let nodes_high = d.dom_nodes.map(|v| v >= c.high_dom_nodes).unwrap_or(false);
            heap_high || nodes_high
        },
        evidence: |d, c| {
            let mut e = Vec::new();
            if let Some(h) = d.js_heap_used_bytes {
                e.push(EvidencePoint {
                    metric: "js_heap_used_bytes".into(),
                    value: format!("{} MB", h / (1024 * 1024)),
                    detail: format!("threshold: >= {} MB", c.high_heap_bytes / (1024 * 1024)),
                });
            }
            if let Some(n) = d.dom_nodes {
                e.push(EvidencePoint {
                    metric: "dom_nodes".into(),
                    value: n.to_string(),
                    detail: format!("threshold: >= {}", c.high_dom_nodes),
                });
            }
            e
        },
    },
    SignalRule {
        kind: SignalKind::RapidHeapGrowth,
        severity: "medium",
        condition: |d, c| {
            d.heap_growth_bytes_per_second
                .map(|v| v > 0 && (v as u64) >= c.heap_growth_bytes_per_second)
                .unwrap_or(false)
        },
        evidence: |d, c| {
            evidence(
                "heap_growth_bytes_per_second",
                format!(
                    "{} MB/s",
                    (d.heap_growth_bytes_per_second.unwrap_or(0) as f64 / (1024.0 * 1024.0))
                ),
                format!(
                    "threshold: >= {} MB/s (sampled over the observation window)",
                    c.heap_growth_bytes_per_second / (1024 * 1024)
                ),
            )
        },
    },
    SignalRule {
        kind: SignalKind::SustainedHeapGrowth,
        severity: "medium",
        condition: |d, c| {
            d.heap_growth_sustained
                && d.heap_growth_bytes_per_second
                    .map(|v| v > 0 && (v as u64) >= c.sustained_growth_bytes_per_second)
                    .unwrap_or(false)
        },
        evidence: |d, c| {
            evidence(
                "heap_growth_sustained",
                format!(
                    "{} MB/s growth repeated across multiple samples",
                    d.heap_growth_bytes_per_second.unwrap_or(0) as f64 / (1024.0 * 1024.0)
                ),
                format!(
                    "threshold: >= {:.1} MB/s with repeated upward movement",
                    c.sustained_growth_bytes_per_second as f64 / (1024.0 * 1024.0)
                ),
            )
        },
    },
    SignalRule {
        kind: SignalKind::HighJsActivity,
        severity: "medium",
        condition: |d, c| d.script_ms >= c.high_script_ms,
        evidence: |d, c| {
            evidence(
                "script_ms",
                format!("{:.0} ms of main-thread script execution", d.script_ms),
                format!("threshold: >= {:.0} ms per window", c.high_script_ms),
            )
        },
    },
    SignalRule {
        kind: SignalKind::ManyLongTasks,
        severity: "medium",
        condition: |d, c| d.long_task_ms >= c.long_task_ms,
        evidence: |d, c| {
            evidence(
                "long_task_ms",
                format!("{:.0} ms of long tasks", d.long_task_ms),
                format!("threshold: >= {:.0} ms per window", c.long_task_ms),
            )
        },
    },
    SignalRule {
        kind: SignalKind::ManyFailedRequests,
        severity: "high",
        condition: |d, c| {
            d.total_requests > 0
                && (d.failed_requests >= c.failed_request_threshold
                    || d.failed_requests as f64 / d.total_requests as f64 >= c.failed_request_ratio)
        },
        evidence: |d, c| {
            let ratio = if d.total_requests == 0 {
                0.0
            } else {
                d.failed_requests as f64 / d.total_requests as f64
            };
            evidence(
                "failed_requests",
                format!(
                    "{} of {} requests failed ({:.0}%)",
                    d.failed_requests,
                    d.total_requests,
                    ratio * 100.0
                ),
                format!(
                    "threshold: >= {} requests or >= {:.0}% ratio",
                    c.failed_request_threshold,
                    c.failed_request_ratio * 100.0
                ),
            )
        },
    },
    SignalRule {
        kind: SignalKind::HighRequestLatency,
        severity: "medium",
        condition: |d, c| {
            d.avg_response_ms
                .map(|v| v >= c.high_latency_ms)
                .unwrap_or(false)
                || d.p95_response_ms
                    .map(|v| v >= c.high_p95_ms)
                    .unwrap_or(false)
        },
        evidence: |d, c| {
            let mut e = Vec::new();
            if let Some(a) = d.avg_response_ms {
                e.push(EvidencePoint {
                    metric: "avg_response_ms".into(),
                    value: format!("{a:.0} ms"),
                    detail: format!("threshold: >= {:.0} ms", c.high_latency_ms),
                });
            }
            if let Some(p) = d.p95_response_ms {
                e.push(EvidencePoint {
                    metric: "p95_response_ms".into(),
                    value: format!("{p:.0} ms"),
                    detail: format!("threshold: >= {:.0} ms", c.high_p95_ms),
                });
            }
            e
        },
    },
    SignalRule {
        kind: SignalKind::HeavyNetworkActivity,
        severity: "low",
        condition: |d, c| {
            d.bytes_transferred
                .map(|v| v >= c.high_network_bytes)
                .unwrap_or(false)
        },
        evidence: |d, c| {
            evidence(
                "bytes_transferred",
                format!(
                    "{} MB transferred",
                    d.bytes_transferred.unwrap_or(0) / (1024 * 1024)
                ),
                format!(
                    "threshold: >= {} MB per window",
                    c.high_network_bytes / (1024 * 1024)
                ),
            )
        },
    },
    SignalRule {
        kind: SignalKind::RuntimeErrors,
        severity: "medium",
        condition: |d, c| d.console_errors + d.exceptions >= c.runtime_error_threshold,
        evidence: |d, c| {
            let mut e = Vec::new();
            if d.console_errors > 0 {
                e.push(EvidencePoint {
                    metric: "console_errors".into(),
                    value: format!("{} console errors", d.console_errors),
                    detail: format!(
                        "threshold: >= {} combined with exceptions",
                        c.runtime_error_threshold
                    ),
                });
            }
            if d.exceptions > 0 {
                e.push(EvidencePoint {
                    metric: "exceptions".into(),
                    value: format!("{} exceptions", d.exceptions),
                    detail: format!(
                        "threshold: >= {} combined with console errors",
                        c.runtime_error_threshold
                    ),
                });
            }
            e
        },
    },
];

/// Compute the signals present in `data`.
pub fn compute_signals(
    data: &TabDiagnosticData,
    config: &DiagnosticsConfig,
) -> Vec<DiagnosticSignal> {
    SIGNAL_RULES
        .iter()
        .filter(|rule| (rule.condition)(data, config))
        .map(|rule| DiagnosticSignal {
            kind: rule.kind.as_str().to_string(),
            label: rule.kind.label().to_string(),
            severity: rule.severity.to_string(),
            evidence: (rule.evidence)(data, config),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sustained_growth_requires_both_rate_and_repeated_movement() {
        let cfg = DiagnosticsConfig::default();
        // Rate alone (single snapshot) must not fire the signal.
        let fast = TabDiagnosticData {
            heap_growth_bytes_per_second: Some(2 * 1024 * 1024),
            ..TabDiagnosticData::default()
        };
        assert!(!compute_signals(&fast, &cfg)
            .iter()
            .any(|s| s.kind == "sustained_heap_growth"));
        let sustained = TabDiagnosticData {
            heap_growth_bytes_per_second: Some(2 * 1024 * 1024),
            heap_growth_sustained: true,
            ..TabDiagnosticData::default()
        };
        let signals = compute_signals(&sustained, &cfg);
        assert!(signals.iter().any(|s| s.kind == "sustained_heap_growth"));
        // Below the sustained threshold, no signal even when repeated.
        let slow = TabDiagnosticData {
            heap_growth_bytes_per_second: Some(128 * 1024),
            heap_growth_sustained: true,
            ..TabDiagnosticData::default()
        };
        assert!(!compute_signals(&slow, &cfg)
            .iter()
            .any(|s| s.kind == "sustained_heap_growth"));
    }
}
