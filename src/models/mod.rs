//! Unified data models shared by providers, tools, and diagnostics.
//!
//! Everything WinKit returns to an MCP client is a serialization of one of
//! these structures. Keeping a single model set lets the Windows layer
//! produce consistent, correlatable output.

pub mod diagnostics;
pub mod event;
pub mod evidence;
pub mod hardware;
pub mod health;
pub mod network;
pub mod process;
pub mod registry;
pub mod service;
pub mod storage;
pub mod system;
pub mod window;

pub use diagnostics::{
    DiagnosticCorrelation, DiagnosticReport, DiagnosticSignal, EvidencePoint, Measurement,
    PossibleCause, RankedFinding, SystemAppEvidence, SystemBatteryEvidence, SystemDiagnosis,
    SystemDiagnosticData, SystemDriveEvidence, SystemStorageHealthEvidence, SystemThermalEvidence,
    SystemWifiEvidence,
};
pub use event::{EventInfo, EventLevel, EventQuery};
pub use evidence::{
    sort_findings, stable_id, DetailLevel, EvidenceConfidence, EvidenceItem, EvidenceSource,
    FindingCategory, FindingConfidence, FindingItem, FindingSeverity, ReportEnvelope, ReportStatus,
};
pub use hardware::{
    BatteryHealth, BatteryInfo, BatteryStatus, CpuHardwareInfo, DiskActivity, DiskHealthReport,
    GpuHardwareInfo, HardwareSnapshot, MemoryHardwareInfo, NetworkAdapterInfo, NetworkDiagnosis,
    NetworkDiagnosticInterface, NetworkFinding, NetworkSnapshot, PowerStateInfo, PowerStatus,
    SensorAvailability, SensorClass, SensorKind, SensorQuality, SensorReading, SensorSource,
    SensorStatus, StorageActivity, StorageDeviceInfo, StorageHealthDevice, ThermalSnapshot,
    ThermalStateSummary, UnavailableReading, WifiAdapterStatus, WifiNetwork, WifiScan,
};
pub use health::{
    ApplicationGroupInfo, DriveHealth, HealthIssue, SystemHealth, SystemHealthReport,
};
pub use network::{ConnectionInfo, NetworkInterfaceInfo, PortInfo, TcpState};
pub use process::{CpuTime, ProcessInfo, ProcessMemory, ProcessOnPort, ProcessTreeNode};
pub use registry::{
    assess_startup_impact, extract_executable_path, InstalledSoftware, RegistryCounts,
    RegistryDiagnostics, StartupProgram, SystemIdentity,
};
pub use service::ServiceInfo;
pub use storage::{DiskUsage, DriveInfo};
pub use system::{
    is_development_port, CpuSnapshot, DevEnvironment, DevServerInfo, DevTool, DiskSnapshotEntry,
    Hotfix, PathAudit, PathEntry, ResourceSnapshot, SystemInfo, UpdateStatus,
    KNOWN_DEV_SERVER_NAMES, KNOWN_DEV_TOOLS,
};
pub use window::WindowInfo;
