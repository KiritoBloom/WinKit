//! Unified data models shared by providers, tools, and diagnostics.
//!
//! Everything WinKit returns to an MCP client is a serialization of one of
//! these structures. Keeping a single model set lets the Windows layer and
//! the application layer (e.g. Chrome) produce consistent, correlatable
//! output.

pub mod browser;
pub mod diagnostics;
pub mod diskscan;
pub mod event;
pub mod evidence;
pub mod health;
pub mod network;
pub mod process;
pub mod service;
pub mod storage;
pub mod system;
pub mod window;

pub use browser::{
    ApplicationInfo, ApplicationState, BrowserInfo, BrowserProcessInfo, ConsoleMessage, MemoryInfo,
    NetworkRequestSummary, NetworkSummary, PerformanceMetrics, RuntimeInfo, TabInfo, TargetInfo,
    TrendInfo, TrendMemory, TrendSample,
};
pub use diagnostics::{
    DiagnosticCorrelation, DiagnosticReport, DiagnosticSignal, EvidencePoint, Measurement,
    PossibleCause, RankedFinding, SystemAppEvidence, SystemDiagnosis, SystemDiagnosticData,
    SystemDriveEvidence, TabDiagnosticData,
};
pub use diskscan::{
    DiskQueryKind, DiskQueryRequest, DiskQueryResult, DiskScanInfo, DiskScanRequest,
    DiskScanStatusInfo, ScanCapacity, ScanFileEntry, ScanFindFile, ScanFolderEntry, ScanFolderSize,
    ScannerKind,
};
pub use event::{EventInfo, EventLevel, EventQuery};
pub use evidence::{
    sort_findings, stable_id, DetailLevel, EvidenceConfidence, EvidenceItem, EvidenceSource,
    FindingCategory, FindingConfidence, FindingItem, FindingSeverity, ReportEnvelope, ReportStatus,
};
pub use health::{
    ApplicationGroupInfo, DriveHealth, HealthIssue, SystemHealth, SystemHealthReport,
};
pub use network::{ConnectionInfo, NetworkInterfaceInfo, PortInfo, TcpState};
pub use process::{CpuTime, ProcessInfo, ProcessMemory, ProcessOnPort, ProcessTreeNode};
pub use service::ServiceInfo;
pub use storage::{DiskUsage, DriveInfo, FileEntry, FindLargeFilesRequest};
pub use system::{
    is_development_port, CpuSnapshot, DevEnvironment, DevServerInfo, DevTool, DiskSnapshotEntry,
    ResourceSnapshot, SystemInfo, KNOWN_DEV_SERVER_NAMES, KNOWN_DEV_TOOLS,
};
pub use window::WindowInfo;
