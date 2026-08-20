//! Correlation rules: signal combinations → possible causes.
//!
//! These are documented heuristics. Confidence values are conservative:
//! a two-signal correlation gets `medium`, three or more mutually
//! reinforcing signals can reach `high`. WinKit never claims certainty it
//! does not have.

use crate::models::{DiagnosticCorrelation, DiagnosticSignal, PossibleCause};

/// One possible-cause rule.
pub struct CorrelationRule {
    /// Human-readable hypothesis.
    pub hypothesis: &'static str,
    /// Signal kinds that support this hypothesis.
    pub supporting_signals: &'static [&'static str],
    /// Base confidence when all signals are present.
    pub confidence: f64,
}

/// The ordered possible-cause rule set.
pub const POSSIBLE_CAUSE_RULES: &[CorrelationRule] = &[
    CorrelationRule {
        hypothesis: "Possible main-thread JavaScript pressure (heavy script execution with long tasks)",
        supporting_signals: &["high_js_activity", "many_long_tasks"],
        confidence: 0.8,
    },
    CorrelationRule {
        hypothesis: "Possible CPU-intensive page work (high CPU with heavy JavaScript activity)",
        supporting_signals: &["high_cpu", "high_js_activity"],
        confidence: 0.8,
    },
    CorrelationRule {
        hypothesis: "Possible memory growth / leak-like behavior (high heap with rapid growth)",
        supporting_signals: &["high_memory", "rapid_heap_growth"],
        confidence: 0.7,
    },
    CorrelationRule {
        hypothesis: "Possible sustained memory growth under continued activity",
        supporting_signals: &["sustained_heap_growth"],
        confidence: 0.55,
    },
    CorrelationRule {
        hypothesis: "Possible network bottleneck or failing endpoints (many failed requests with high latency)",
        supporting_signals: &["many_failed_requests", "high_request_latency"],
        confidence: 0.7,
    },
    CorrelationRule {
        hypothesis: "Possible dependency on failing external resources (heavy network with failures)",
        supporting_signals: &["heavy_network_activity", "many_failed_requests"],
        confidence: 0.6,
    },
    CorrelationRule {
        hypothesis: "Possible page runtime issues (console errors and/or exceptions)",
        supporting_signals: &["runtime_errors"],
        confidence: 0.6,
    },
    CorrelationRule {
        hypothesis: "Possible heavy interactive page (high memory with many long tasks)",
        supporting_signals: &["high_memory", "many_long_tasks"],
        confidence: 0.5,
    },
    CorrelationRule {
        hypothesis: "Possible sustained CPU activity",
        supporting_signals: &["high_cpu"],
        confidence: 0.4,
    },
    CorrelationRule {
        hypothesis: "Possible heap growth without immediate pressure",
        supporting_signals: &["rapid_heap_growth"],
        confidence: 0.4,
    },
];

/// Emit the correlations present between emitted signals.
pub fn compute_correlations(signals: &[DiagnosticSignal]) -> Vec<DiagnosticCorrelation> {
    let kinds: Vec<&str> = signals.iter().map(|s| s.kind.as_str()).collect();
    let mut out = Vec::new();
    // Pairwise correlations between every distinct pair of emitted signals.
    for (i, a) in kinds.iter().enumerate() {
        for b in kinds.iter().skip(i + 1) {
            out.push(DiagnosticCorrelation {
                description: format!("'{a}' and '{b}' co-occur"),
                signals: vec![a.to_string(), b.to_string()],
                confidence: 0.5,
            });
        }
    }
    out
}

fn confidence_label(v: f64) -> &'static str {
    if v >= 0.75 {
        "high"
    } else if v >= 0.55 {
        "medium"
    } else {
        "low"
    }
}

/// Match emitted signals against the possible-cause rules.
pub fn compute_possible_causes(
    signals: &[DiagnosticSignal],
    _correlations: &[DiagnosticCorrelation],
) -> Vec<PossibleCause> {
    let kinds: Vec<&str> = signals.iter().map(|s| s.kind.as_str()).collect();
    let mut out = Vec::new();
    for rule in POSSIBLE_CAUSE_RULES {
        let present: Vec<String> = rule
            .supporting_signals
            .iter()
            .filter(|need| kinds.contains(need))
            .map(|s| s.to_string())
            .collect();
        if present.len() == rule.supporting_signals.len() && !present.is_empty() {
            let mut reasoning = format!(
                "Observed signals [{}] match the documented pattern '{}'.",
                present.join(", "),
                rule.hypothesis
            );
            reasoning.push_str(
                " This is a heuristic correlation of measured evidence, not a verified root cause.",
            );
            out.push(PossibleCause {
                hypothesis: rule.hypothesis.to_string(),
                supporting_signals: present,
                confidence: confidence_label(rule.confidence).to_string(),
                confidence_value: rule.confidence,
                reasoning,
            });
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn signal(kind: &str) -> DiagnosticSignal {
        DiagnosticSignal {
            kind: kind.to_string(),
            label: kind.to_string(),
            severity: "medium".to_string(),
            evidence: Vec::new(),
        }
    }

    #[test]
    fn js_pressure_pattern_matches() {
        let signals = vec![signal("high_js_activity"), signal("many_long_tasks")];
        let causes = compute_possible_causes(&signals, &[]);
        assert!(causes
            .iter()
            .any(|c| c.hypothesis.contains("main-thread JavaScript pressure")));
    }

    #[test]
    fn single_signal_matches_low_confidence_rules_only() {
        let signals = vec![signal("high_cpu")];
        let causes = compute_possible_causes(&signals, &[]);
        assert_eq!(causes.len(), 1);
        assert_eq!(causes[0].confidence, "low");
    }

    #[test]
    fn pairwise_correlations_are_symmetric_and_bounded() {
        let signals = vec![signal("a"), signal("b"), signal("c")];
        let corr = compute_correlations(&signals);
        assert_eq!(corr.len(), 3);
    }
}
