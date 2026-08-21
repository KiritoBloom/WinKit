//! Shared evidence, finding, and report-envelope models.
//!
//! High-level workflow tools (`diagnose_workspace`, `diagnose_local_webapp`,
//! ...) return one [`ReportEnvelope`] instead of unrelated ad hoc JSON:
//! every observation is an [`EvidenceItem`] with a stable ID, every
//! conclusion is a [`FindingItem`] that cites evidence IDs, and detail levels
//! are deliberate projections of the same report.
//!
//! Language rules: *confirmed* = the probe directly observed the
//! condition; *observed* = the provider returned a signal without proving
//! the underlying cause; *likely* = multiple supporting observations;
//! *possible* = weakly supported; *unknown* = evidence unavailable. WinKit
//! never claims causality from timing proximity alone.

use crate::utils::truncate;
use serde::{Deserialize, Serialize};

/// FNV-1a 64-bit hash over the input parts, formatted as hex. Stable across
/// builds and platforms so evidence/finding IDs are deterministic.
pub fn stable_id(prefix: &str, parts: &[&str]) -> String {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for part in parts {
        for byte in part.as_bytes() {
            hash ^= *byte as u64;
            hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
        }
        hash ^= 0xff;
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    format!("{prefix}-{hash:016x}")
}

/// Where an evidence item came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EvidenceSource {
    WorkspaceMetadata,
    RepositoryMetadata,
    ProcessInspection,
    PortListener,
    HttpProbe,
    WindowsEvents,
    ServiceState,
    SystemHealth,
}

impl EvidenceSource {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::WorkspaceMetadata => "workspace_metadata",
            Self::RepositoryMetadata => "repository_metadata",
            Self::ProcessInspection => "process_inspection",
            Self::PortListener => "port_listener",
            Self::HttpProbe => "http_probe",
            Self::WindowsEvents => "windows_events",
            Self::ServiceState => "service_state",
            Self::SystemHealth => "system_health",
        }
    }
}

/// How certain an evidence item is about what it observes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EvidenceConfidence {
    /// The probe directly observed the condition.
    Confirmed,
    /// The provider returned a signal without proving the underlying cause.
    Observed,
    /// Multiple supporting observations make the explanation plausible.
    Likely,
    /// The explanation is compatible with the evidence but weakly supported.
    Possible,
    /// Required evidence was unavailable.
    Unknown,
}

impl EvidenceConfidence {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Confirmed => "confirmed",
            Self::Observed => "observed",
            Self::Likely => "likely",
            Self::Possible => "possible",
            Self::Unknown => "unknown",
        }
    }
}

/// One observation collected by a high-level tool.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct EvidenceItem {
    /// Stable evidence ID, e.g. `ev-<hash>`; deterministic for the same
    /// source+subject so agents can correlate across calls.
    pub id: String,
    /// Source category.
    pub source: EvidenceSource,
    /// Redacted subject, e.g. `Port 3000`, `node.exe (PID 900)`.
    pub subject: String,
    /// RFC3339 observation time, when available.
    pub observed_at: Option<String>,
    /// Bounded structured observation.
    pub value: serde_json::Value,
    /// Reliability marker.
    pub confidence: EvidenceConfidence,
    /// Why this item is partial, when it is.
    pub limitation: Option<String>,
}

impl EvidenceItem {
    /// Build an evidence item with a deterministic id.
    pub fn new(
        source: EvidenceSource,
        subject: impl Into<String>,
        value: serde_json::Value,
        confidence: EvidenceConfidence,
        limitation: Option<String>,
    ) -> Self {
        let subject = subject.into();
        Self {
            id: stable_id("ev", &[source.as_str(), subject.as_str()]),
            source,
            subject: truncate(&subject, 160),
            observed_at: crate::utils::time::format_rfc3339_opt(std::time::SystemTime::now()),
            value,
            confidence,
            limitation,
        }
    }
}

/// Finding severity.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FindingSeverity {
    Info,
    Low,
    Medium,
    High,
    Critical,
}

impl FindingSeverity {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Info => "info",
            Self::Low => "low",
            Self::Medium => "medium",
            Self::High => "high",
            Self::Critical => "critical",
        }
    }
}

/// Finding confidence.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FindingConfidence {
    Confirmed,
    Observed,
    Likely,
    Possible,
    Unknown,
}

impl FindingConfidence {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Confirmed => "confirmed",
            Self::Observed => "observed",
            Self::Likely => "likely",
            Self::Possible => "possible",
            Self::Unknown => "unknown",
        }
    }
}

/// Finding category.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FindingCategory {
    Server,
    Port,
    Process,
    Workspace,
    System,
    Unknown,
}

impl FindingCategory {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Server => "server",
            Self::Port => "port",
            Self::Process => "process",
            Self::Workspace => "workspace",
            Self::System => "system",
            Self::Unknown => "unknown",
        }
    }
}

/// One evidence-backed conclusion.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct FindingItem {
    /// Stable finding ID, e.g. `find-port-unrelated-process`.
    pub id: String,
    pub severity: FindingSeverity,
    pub title: String,
    /// Plain-language explanation.
    pub explanation: String,
    /// Evidence IDs supporting this finding.
    pub supporting_evidence: Vec<String>,
    /// Evidence IDs that point away from this finding, when applicable.
    pub contradicting_evidence: Vec<String>,
    pub confidence: FindingConfidence,
    pub category: FindingCategory,
    /// Tools the agent should call next.
    pub recommended_next_tools: Vec<String>,
    /// What would confirm or disprove this finding.
    pub confirm_disprove: String,
}

impl FindingItem {
    pub fn new(
        id: &str,
        severity: FindingSeverity,
        title: impl Into<String>,
        explanation: impl Into<String>,
        confidence: FindingConfidence,
        category: FindingCategory,
    ) -> Self {
        Self {
            id: id.to_string(),
            severity,
            title: title.into(),
            explanation: explanation.into(),
            supporting_evidence: Vec::new(),
            contradicting_evidence: Vec::new(),
            confidence,
            category,
            recommended_next_tools: Vec::new(),
            confirm_disprove: String::new(),
        }
    }
}

/// Report status.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReportStatus {
    Ok,
    IssuesDetected,
    NoSupportedSignalDetected,
    Limited,
    Blocked,
}

impl ReportStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Ok => "ok",
            Self::IssuesDetected => "issues_detected",
            Self::NoSupportedSignalDetected => "no_supported_signal_detected",
            Self::Limited => "limited",
            Self::Blocked => "blocked",
        }
    }
}

/// Detail level of a report. `compact` is a deliberate projection of
/// `normal`, which is a deliberate projection of `detailed`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DetailLevel {
    Compact,
    Normal,
    Detailed,
}

impl DetailLevel {
    pub fn parse(s: &str) -> Option<Self> {
        match s.to_ascii_lowercase().as_str() {
            "compact" => Some(Self::Compact),
            "normal" => Some(Self::Normal),
            "detailed" => Some(Self::Detailed),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Compact => "compact",
            Self::Normal => "normal",
            Self::Detailed => "detailed",
        }
    }
}

/// The stable envelope returned by every high-level workflow tool.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ReportEnvelope {
    pub schema_version: String,
    pub status: ReportStatus,
    pub summary: String,
    pub findings: Vec<FindingItem>,
    pub evidence: Vec<EvidenceItem>,
    /// Dimensions that were checked and found clean.
    pub checked: Vec<String>,
    pub recommended_next_tools: Vec<String>,
    pub limitations: Vec<String>,
    pub generated_at: String,
    pub duration_ms: u64,
    pub detail_level: DetailLevel,
}

impl ReportEnvelope {
    /// Start a new envelope with the standard header fields filled in.
    pub fn begin(
        status: ReportStatus,
        summary: impl Into<String>,
        detail_level: DetailLevel,
    ) -> Self {
        Self {
            schema_version: "1".to_string(),
            status,
            summary: summary.into(),
            findings: Vec::new(),
            evidence: Vec::new(),
            checked: Vec::new(),
            recommended_next_tools: Vec::new(),
            limitations: Vec::new(),
            generated_at: crate::utils::time::format_rfc3339(std::time::SystemTime::now()),
            duration_ms: 0,
            detail_level,
        }
    }

    /// `true` when a provider failed to return core evidence, making the
    /// report weaker than usual.
    pub fn mark_limited(&mut self, limitation: impl Into<String>) {
        self.status = ReportStatus::Limited;
        self.limitations.push(limitation.into());
    }

    /// Project this report onto the requested detail level. This is a
    /// deliberate structural projection, never a blind string truncation.
    pub fn project(&self, level: DetailLevel) -> serde_json::Value {
        let mut findings: Vec<serde_json::Value> = self
            .findings
            .iter()
            .map(|f| {
                serde_json::json!({
                    "id": f.id,
                    "severity": f.severity,
                    "title": f.title,
                    "explanation": f.explanation,
                    "confidence": f.confidence,
                    "category": f.category,
                    "supporting_evidence": f.supporting_evidence,
                    "contradicting_evidence": f.contradicting_evidence,
                })
            })
            .collect();

        // Compact keeps only the top findings and the evidence they cite.
        let kept_evidence: std::collections::BTreeSet<&str> = findings
            .iter()
            .take(findings.len().min(3))
            .flat_map(|f| {
                f.get("supporting_evidence")
                    .and_then(|v| v.as_array())
                    .into_iter()
                    .flatten()
                    .filter_map(|v| v.as_str())
            })
            .collect();
        let mut evidence: Vec<serde_json::Value> = self
            .evidence
            .iter()
            .filter(|e| level != DetailLevel::Compact || kept_evidence.contains(e.id.as_str()))
            .map(|e| {
                serde_json::json!({
                    "id": e.id,
                    "source": e.source,
                    "subject": e.subject,
                    "value": e.value,
                    "confidence": e.confidence,
                    "limitation": e.limitation,
                })
            })
            .collect();
        if level == DetailLevel::Compact {
            findings.truncate(3);
            evidence.truncate(12);
        }

        // Detailed reports keep provider notes attached to evidence values
        // (already embedded in `value`), plus everything else.
        let base = serde_json::json!({
            "schema_version": self.schema_version,
            "status": self.status,
            "summary": self.summary,
            "findings": findings,
            "checked": self.checked,
            "recommended_next_tools": self.recommended_next_tools,
            "limitations": self.limitations,
            "generated_at": self.generated_at,
            "duration_ms": self.duration_ms,
            "detail_level": level,
        });
        let mut obj = base.as_object().cloned().unwrap_or_default();
        if level != DetailLevel::Compact {
            obj.insert("evidence".to_string(), serde_json::Value::Array(evidence));
        }
        serde_json::Value::Object(obj)
    }
}

/// Sort findings by severity (critical first) then by stable id, so ranking
/// is deterministic across calls and platforms.
pub fn sort_findings(findings: &mut [FindingItem]) {
    findings.sort_by(|a, b| b.severity.cmp(&a.severity).then_with(|| a.id.cmp(&b.id)));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stable_ids_are_deterministic() {
        let a = stable_id("ev", &["port_listener", "3000"]);
        let b = stable_id("ev", &["port_listener", "3000"]);
        let c = stable_id("ev", &["port_listener", "3001"]);
        assert_eq!(a, b);
        assert_ne!(a, c);
        assert!(a.starts_with("ev-"));
    }

    #[test]
    fn envelope_projection_respects_levels() {
        let mut report = ReportEnvelope::begin(
            ReportStatus::IssuesDetected,
            "port 3000 is owned by an unrelated process",
            DetailLevel::Normal,
        );
        let ev = EvidenceItem::new(
            EvidenceSource::PortListener,
            "Port 3000",
            serde_json::json!({ "pid": 900, "process_name": "node.exe" }),
            EvidenceConfidence::Confirmed,
            None,
        );
        let ev_id = ev.id.clone();
        report.evidence.push(ev);
        let mut finding = FindingItem::new(
            "port-unrelated-process",
            FindingSeverity::High,
            "Port 3000 is owned by an unrelated process",
            "PID 900 is not related to the workspace.",
            FindingConfidence::Confirmed,
            FindingCategory::Port,
        );
        finding.supporting_evidence.push(ev_id.clone());
        report.findings.push(finding);

        let compact = report.project(DetailLevel::Compact);
        assert_eq!(compact["detail_level"], "compact");
        assert_eq!(compact["summary"], report.summary);
        assert_eq!(compact["findings"].as_array().unwrap().len(), 1);
        assert!(compact.get("evidence").is_none());

        let normal = report.project(DetailLevel::Normal);
        assert_eq!(normal["evidence"].as_array().unwrap().len(), 1);

        let detailed = report.project(DetailLevel::Detailed);
        assert_eq!(detailed["evidence"].as_array().unwrap().len(), 1);
        assert_eq!(detailed["schema_version"], "1");
    }

    #[test]
    fn serialization_uses_snake_case_enum_names() {
        let s = serde_json::to_string(&DetailLevel::Compact).unwrap();
        assert_eq!(s, "\"compact\"");
        assert_eq!(
            serde_json::to_string(&EvidenceSource::HttpProbe).unwrap(),
            "\"http_probe\""
        );
        assert_eq!(
            serde_json::to_string(&FindingSeverity::Critical).unwrap(),
            "\"critical\""
        );
    }

    #[test]
    fn detail_level_parse_covers_documented_names() {
        assert_eq!(DetailLevel::parse("compact"), Some(DetailLevel::Compact));
        assert_eq!(DetailLevel::parse("NORMAL"), Some(DetailLevel::Normal));
        assert_eq!(DetailLevel::parse("detailed"), Some(DetailLevel::Detailed));
        assert_eq!(DetailLevel::parse("nonsense"), None);
    }

    #[test]
    fn sort_findings_ranks_critical_first() {
        let mut findings = vec![
            FindingItem::new(
                "b",
                FindingSeverity::Low,
                "low",
                "low",
                FindingConfidence::Observed,
                FindingCategory::Unknown,
            ),
            FindingItem::new(
                "a",
                FindingSeverity::Critical,
                "crit",
                "crit",
                FindingConfidence::Confirmed,
                FindingCategory::Port,
            ),
        ];
        sort_findings(&mut findings);
        assert_eq!(findings[0].id, "a");
        assert_eq!(findings[1].id, "b");
    }
}
