//! Hardware telemetry models: sensors, thermal state, power/battery,
//! storage health, and Wi-Fi observability.
//!
//! The guiding rule for every reading here is honesty: a sensor value is
//! either measured or explicitly reported as unavailable with a reason.
//! "No sensor" is not "healthy". WinKit never invents numbers, and the
//! documentation for each tool explains exactly which Windows API produced
//! each field.

use serde::{Deserialize, Serialize};

use crate::models::diagnostics::EvidencePoint;

/// Where a sensor value comes from, so an agent can judge its trustworthiness.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SensorSource {
    /// Direct Windows API result, e.g. `GetSystemPowerStatus`.
    WindowsApi,
    /// `MSAcpi_ThermalZoneTemperature` via WMI (root\WMI, tenths of Kelvin).
    ThermalZone,
    /// Performance counter (PDH), e.g. `\Processor Information(_Total)\% Processor Performance`.
    PerformanceCounter,
    /// Generic WMI query, e.g. `Win32_VideoController` or `Win32_Processor`.
    Wmi,
    /// CPU vendor discovery (Intel/AMD model-specific registers).
    CpuVendor,
    /// GPU vendor API (NVML/ADL-style vendor telemetry).
    GpuVendor,
    /// NVMe S.M.A.R.T. via storage protocol-specific property.
    NvmeSmart,
    /// ATA S.M.A.R.T. via pass-through.
    AtaSmart,
    /// Battery/power subsystem API.
    BatteryApi,
    /// Native Wi-Fi API (`WlanEnumInterfaces` / `WlanQueryInterface`).
    WlanApi,
    /// The value could not be attributed to a specific source.
    Unknown,
}

impl SensorSource {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::WindowsApi => "windows_api",
            Self::ThermalZone => "thermal_zone",
            Self::PerformanceCounter => "performance_counter",
            Self::Wmi => "wmi",
            Self::CpuVendor => "cpu_vendor",
            Self::GpuVendor => "gpu_vendor",
            Self::NvmeSmart => "nvme_smart",
            Self::AtaSmart => "ata_smart",
            Self::BatteryApi => "battery_api",
            Self::WlanApi => "wlan_api",
            Self::Unknown => "unknown",
        }
    }
}

/// Availability of a sensor reading. The default state of anything WinKit
/// could not measure is `Unavailable` with an explicit reason — never a
/// fabricated value and never a silent omission.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum SensorAvailability {
    /// The value was measured successfully.
    Available,
    /// The value could not be read; `reason` explains why.
    #[default]
    Unavailable,
    /// No supported Windows API path exists for this sensor.
    Unsupported,
    /// The hardware is not present (e.g. no battery on a desktop).
    NotPresent,
    /// Reading the sensor requires elevation and the process is not elevated.
    PermissionDenied,
    /// The driver/provider the API depends on is not installed.
    DriverMissing,
    /// The read failed transiently; a retry may succeed.
    TransientFailure,
}

impl SensorAvailability {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Available => "available",
            Self::Unavailable => "unavailable",
            Self::Unsupported => "unsupported",
            Self::NotPresent => "not_present",
            Self::PermissionDenied => "permission_denied",
            Self::DriverMissing => "driver_missing",
            Self::TransientFailure => "transient_failure",
        }
    }

    pub fn is_available(self) -> bool {
        self == Self::Available
    }
}

/// Data quality of a measured value.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SensorQuality {
    /// High-confidence reading from a directly measured source.
    High,
    /// Indirect or vendor-derived reading with reasonable confidence.
    Medium,
    /// Derived, inferred, or stale reading; treat with caution.
    Low,
    /// Quality could not be determined.
    Unknown,
}

/// Interpreted status of a sensor relative to its component's thresholds.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SensorStatus {
    Ok,
    Warning,
    Critical,
    Unknown,
    /// The sensor is absent; status does not apply.
    NotApplicable,
}

/// The physical class of the component a sensor belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SensorClass {
    CpuPackage,
    CpuCore,
    Gpu,
    Nvme,
    Sata,
    Battery,
    ThermalZone,
    Fan,
    Motherboard,
    Power,
    Voltage,
    Other,
}

/// What a sensor measures (the physical quantity).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SensorKind {
    Temperature,
    FanSpeed,
    Voltage,
    Current,
    Power,
    ClockRate,
    Utilization,
    Percentage,
    Energy,
}

/// One measured or explicitly-unavailable sensor reading.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SensorReading {
    /// Stable id within a session, e.g. `cpu_package-0` or `thermal_zone-0`.
    pub sensor_id: String,
    pub name: String,
    pub class: SensorClass,
    pub kind: SensorKind,
    /// Physical component, e.g. `PhysicalDrive0` or `Battery 0`.
    pub component: String,
    /// The measured value. `None` when `availability` is not `available`.
    pub value: Option<f64>,
    /// Explicit unit when a value exists, e.g. `temperature_c`, `percent`,
    /// `mhz`, `rpm`, `volts`.
    pub unit: Option<String>,
    pub source: SensorSource,
    pub availability: SensorAvailability,
    /// Human-readable explanation when the sensor is not available.
    pub reason: Option<String>,
    /// RFC3339 time this reading was taken.
    pub timestamp: String,
    pub quality: SensorQuality,
    pub min: Option<f64>,
    pub max: Option<f64>,
    pub status: SensorStatus,
}

impl SensorReading {
    /// Build an available reading.
    #[allow(clippy::too_many_arguments)]
    pub fn available(
        sensor_id: impl Into<String>,
        name: impl Into<String>,
        class: SensorClass,
        kind: SensorKind,
        component: impl Into<String>,
        value: f64,
        unit: impl Into<String>,
        source: SensorSource,
        quality: SensorQuality,
        min: Option<f64>,
        max: Option<f64>,
    ) -> Self {
        Self {
            sensor_id: sensor_id.into(),
            name: name.into(),
            class,
            kind,
            component: component.into(),
            value: Some(value),
            unit: Some(unit.into()),
            source,
            availability: SensorAvailability::Available,
            reason: None,
            timestamp: crate::utils::time::format_rfc3339(std::time::SystemTime::now()),
            quality,
            min,
            max,
            status: SensorStatus::Unknown,
        }
    }

    /// Build an unavailable reading with an explicit reason.
    pub fn unavailable(
        sensor_id: impl Into<String>,
        name: impl Into<String>,
        class: SensorClass,
        kind: SensorKind,
        component: impl Into<String>,
        availability: SensorAvailability,
        reason: impl Into<String>,
    ) -> Self {
        Self {
            sensor_id: sensor_id.into(),
            name: name.into(),
            class,
            kind,
            component: component.into(),
            value: None,
            unit: None,
            source: SensorSource::Unknown,
            availability,
            reason: Some(reason.into()),
            timestamp: crate::utils::time::format_rfc3339(std::time::SystemTime::now()),
            quality: SensorQuality::Unknown,
            min: None,
            max: None,
            status: SensorStatus::NotApplicable,
        }
    }
}

/// A component whose reading is unavailable, kept in a compact shape so
/// reports can enumerate what they could not measure.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct UnavailableReading {
    /// Component, e.g. `cpu_package` or `PhysicalDrive1`.
    pub component: String,
    /// What was being read, e.g. `temperature` or `health`.
    pub kind: String,
    pub availability: SensorAvailability,
    /// Human-readable explanation.
    pub reason: String,
}

impl UnavailableReading {
    pub fn new(
        component: impl Into<String>,
        kind: impl Into<String>,
        availability: SensorAvailability,
        reason: impl Into<String>,
    ) -> Self {
        Self {
            component: component.into(),
            kind: kind.into(),
            availability,
            reason: reason.into(),
        }
    }
}

// ---------------------------------------------------------------------------
// Thermal snapshot
// ---------------------------------------------------------------------------

/// Summary of the machine's thermal state, with honest uncertainty.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ThermalStateSummary {
    /// `likely`, `not_observed`, or `unknown`.
    pub cpu_throttling: String,
    /// `likely`, `not_observed`, or `unknown`.
    pub gpu_throttling: String,
    /// `low`, `elevated`, `high`, or `unknown`.
    pub cpu_thermal_pressure: String,
    /// `low`, `elevated`, `high`, or `unknown`.
    pub gpu_thermal_pressure: String,
    /// True when the CPU is running well below its base clock, when known.
    pub cpu_frequency_reduced: Option<bool>,
    /// The observations backing each conclusion.
    pub evidence: Vec<EvidencePoint>,
    /// What this snapshot cannot determine.
    pub limitations: Vec<String>,
}

impl Default for ThermalStateSummary {
    fn default() -> Self {
        Self {
            cpu_throttling: "unknown".into(),
            gpu_throttling: "unknown".into(),
            cpu_thermal_pressure: "unknown".into(),
            gpu_thermal_pressure: "unknown".into(),
            cpu_frequency_reduced: None,
            evidence: Vec::new(),
            limitations: Vec::new(),
        }
    }
}

/// Thermal state of the machine: every temperature sensor that could be read,
/// plus a deterministic interpretation.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ThermalSnapshot {
    /// `ok`, `degraded`, `limited`, or `no_sensors`.
    pub status: String,
    pub timestamp: String,
    pub duration_ms: u64,
    /// Temperature sensors only. Sorted by `sensor_id` for determinism.
    pub sensors: Vec<SensorReading>,
    pub thermal_state: ThermalStateSummary,
    /// `full` when every documented sensor source was attempted, `limited`
    /// when a provider failed entirely.
    pub completeness: String,
    pub unavailable: Vec<UnavailableReading>,
    pub warnings: Vec<String>,
}

// ---------------------------------------------------------------------------
// Hardware snapshot
// ---------------------------------------------------------------------------

/// CPU hardware information.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct CpuHardwareInfo {
    /// Processor brand string, e.g. `Intel(R) Core(TM) i7-10700K`.
    pub name: Option<String>,
    /// Vendor, e.g. `GenuineIntel` or `AuthenticAMD`.
    pub vendor: Option<String>,
    pub family: Option<u32>,
    pub model: Option<u32>,
    pub stepping: Option<u32>,
    /// Physical cores.
    pub cores: Option<u32>,
    /// Logical processors (threads).
    pub logical_processors: Option<u32>,
    pub base_clock_mhz: Option<f64>,
    /// Current clock, from `\Processor Information(_Total)\% Processor Performance`.
    pub current_clock_mhz: Option<f64>,
    /// Total CPU utilization as percent of system capacity (0-100).
    pub utilization_percent: Option<f64>,
    /// Package temperature when a `cpu_package` temperature sensor exists.
    pub package_temperature_c: Option<f64>,
    /// Where `package_temperature_c` came from.
    pub temperature_source: Option<String>,
}

/// GPU hardware information.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct GpuHardwareInfo {
    /// Adapter description, e.g. `NVIDIA GeForce RTX 3070`.
    pub name: Option<String>,
    /// `nvidia`, `amd`, `intel`, or `unknown`.
    pub vendor: String,
    pub driver_version: Option<String>,
    pub video_memory_bytes: Option<u64>,
    /// GPU temperature is only reported when a vendor-supported source
    /// exists. `available=false` means WinKit has no documented way to read
    /// it, not that the GPU is cool.
    pub temperature_available: bool,
    pub temperature_c: Option<f64>,
    pub temperature_reason: Option<String>,
}

/// System memory hardware information.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct MemoryHardwareInfo {
    pub total_bytes: Option<u64>,
    /// Number of installed physical memory modules.
    pub module_count: Option<u32>,
}

/// A physical storage device (not a volume).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct StorageDeviceInfo {
    /// Device name, e.g. `PhysicalDrive0`.
    pub device: String,
    pub model: Option<String>,
    /// `nvme`, `sata`, `usb`, or `unknown`.
    pub interface: String,
    pub capacity_bytes: Option<u64>,
    /// `true` when this device is the system/boot disk.
    pub is_system: bool,
}

/// A network adapter relevant to hardware telemetry.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct NetworkAdapterInfo {
    pub index: u32,
    pub name: String,
    pub description: String,
    pub mac_address: Option<String>,
    pub is_wifi: bool,
    pub is_up: bool,
    pub ipv4_addresses: Vec<String>,
    pub gateway: Option<String>,
}

/// Battery hardware information.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct BatteryInfo {
    pub present: bool,
    pub percent: Option<u8>,
    pub ac_online: Option<bool>,
    pub charging: Option<bool>,
    pub estimated_time_remaining_seconds: Option<u64>,
}

/// Current power state.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct PowerStateInfo {
    /// `ac`, `battery`, or `unknown`.
    pub power_source: String,
    pub ac_online: Option<bool>,
    pub battery_present: bool,
    pub battery_percent: Option<u8>,
    /// `charging`, `discharging`, `critical`, `low`, or `unknown`.
    pub battery_state: Option<String>,
    pub charging: Option<bool>,
    pub estimated_time_remaining_seconds: Option<u64>,
}

/// A complete, bounded hardware snapshot of the machine.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct HardwareSnapshot {
    /// `ok`, `limited`, or `unavailable`.
    pub status: String,
    pub timestamp: String,
    pub duration_ms: u64,
    pub cpu: CpuHardwareInfo,
    pub gpus: Vec<GpuHardwareInfo>,
    pub memory: MemoryHardwareInfo,
    pub storage: Vec<StorageDeviceInfo>,
    pub network_adapters: Vec<NetworkAdapterInfo>,
    pub battery: Option<BatteryInfo>,
    pub power_state: PowerStateInfo,
    /// Every sensor collected for this snapshot.
    pub sensors: Vec<SensorReading>,
    /// `full` or `limited`.
    pub completeness: String,
    pub unavailable: Vec<UnavailableReading>,
}

// ---------------------------------------------------------------------------
// Power / battery
// ---------------------------------------------------------------------------

/// Battery health information, where the platform exposes it.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct BatteryHealth {
    pub designed_capacity_mwh: Option<u64>,
    pub full_charge_capacity_mwh: Option<u64>,
    pub current_charge_mwh: Option<u64>,
    pub cycle_count: Option<u32>,
    /// full_charge / designed, as a percentage.
    pub health_percent: Option<f64>,
    pub temperature_c: Option<f64>,
    pub availability: SensorAvailability,
    pub reason: Option<String>,
}

/// Detailed battery status (tool `battery_status`).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct BatteryStatus {
    /// `ok`, `limited`, or `unavailable`.
    pub status: String,
    pub timestamp: String,
    pub present: bool,
    pub percent: Option<u8>,
    pub ac_online: Option<bool>,
    pub charging: Option<bool>,
    /// `charging`, `discharging`, `critical`, `low`, or `unknown`.
    pub battery_state: Option<String>,
    pub estimated_time_remaining_seconds: Option<u64>,
    pub health: Option<BatteryHealth>,
    pub unavailable: Vec<UnavailableReading>,
}

/// Power source status (tool `power_status`).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PowerStatus {
    /// `ok`, `limited`, or `unavailable`.
    pub status: String,
    pub timestamp: String,
    /// `ac`, `battery`, or `unknown`.
    pub power_source: String,
    pub ac_online: Option<bool>,
    pub battery_present: bool,
    pub battery_percent: Option<u8>,
    /// `charging`, `discharging`, `critical`, `low`, or `unknown`.
    pub battery_state: Option<String>,
    pub charging: Option<bool>,
    pub estimated_time_remaining_seconds: Option<u64>,
    pub unavailable: Vec<UnavailableReading>,
}

// ---------------------------------------------------------------------------
// Storage health
// ---------------------------------------------------------------------------

/// Health of one physical storage device.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct StorageHealthDevice {
    /// Device name, e.g. `PhysicalDrive0`.
    pub device: String,
    pub model: Option<String>,
    /// `nvme`, `sata`, `usb`, or `unknown`.
    pub interface: String,
    /// Overall health: `healthy`, `warning`, `critical`, or `unknown`.
    pub health_status: Option<String>,
    pub temperature_c: Option<f64>,
    /// NVMe Critical Warning bitfield descriptions, e.g. `reliability_degraded`.
    pub critical_warning: Vec<String>,
    /// NVMe percentage used (0-100).
    pub percentage_used: Option<u8>,
    /// NVMe available spare (0-100).
    pub available_spare: Option<u8>,
    pub available_spare_threshold: Option<u8>,
    /// NVMe media and data integrity errors.
    pub media_errors: Option<u64>,
    /// NVMe controller power-on hours.
    pub power_on_hours: Option<u64>,
    pub unsafe_shutdowns: Option<u64>,
    pub data_units_read: Option<u64>,
    pub data_units_written: Option<u64>,
    /// ATA reallocated sector count (only when an ATA SMART path works).
    pub reallocated_sectors: Option<u64>,
    pub availability: SensorAvailability,
    /// Why health is unavailable, when it is.
    pub reason: Option<String>,
}

/// Storage health report (tool `disk_health`).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct DiskHealthReport {
    /// `healthy`, `warning`, `critical`, `unknown`, or `not_applicable`.
    pub status: String,
    pub timestamp: String,
    pub duration_ms: u64,
    pub devices: Vec<StorageHealthDevice>,
    /// `full` or `limited`.
    pub completeness: String,
    pub unavailable: Vec<UnavailableReading>,
}

/// Storage activity sampled over a short window (tool `disk_performance`).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct StorageActivity {
    /// `ok`, `degraded`, `limited`, or `unavailable`.
    pub status: String,
    pub timestamp: String,
    /// Milliseconds the sample window was requested to cover (the actual
    /// elapsed time may differ slightly; the requested window is reported so
    /// callers can reason about the sampling period).
    pub sample_window_ms: u64,
    /// One entry per physical disk that reported counters.
    pub disks: Vec<DiskActivity>,
    pub completeness: String,
    pub unavailable: Vec<UnavailableReading>,
}

/// Activity counters for one physical disk.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct DiskActivity {
    pub device: String,
    /// Percent of time the disk was busy during the sample window (0-100).
    pub busy_percent: Option<f64>,
    /// Average queue depth during the window.
    pub avg_queue_depth: Option<f64>,
    pub read_bytes_per_second: Option<f64>,
    pub write_bytes_per_second: Option<f64>,
    pub read_per_second: Option<f64>,
    pub write_per_second: Option<f64>,
    pub availability: SensorAvailability,
    pub reason: Option<String>,
}

// ---------------------------------------------------------------------------
// Wi-Fi
// ---------------------------------------------------------------------------

/// Status of one Wi-Fi adapter.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct WifiAdapterStatus {
    /// Interface GUID, e.g. `{a1b2...}`.
    pub adapter_id: String,
    pub description: String,
    /// `connected`, `disconnected`, or `not_available`.
    pub state: String,
    pub ssid: Option<String>,
    /// Signal quality as reported by the OS (0-100).
    pub signal_percent: Option<u8>,
    /// RSSI in dBm when the OS exposes it.
    pub rssi_dbm: Option<i32>,
    pub link_speed_mbps: Option<f64>,
    pub channel: Option<u32>,
    pub frequency_mhz: Option<u64>,
    /// `2.4ghz`, `5ghz`, `6ghz`, or `unknown`.
    pub band: Option<String>,
    pub authentication: Option<String>,
    pub cipher: Option<String>,
    pub is_up: bool,
}

/// One network from a Wi-Fi scan.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct WifiNetwork {
    pub ssid: Option<String>,
    pub bssid: Option<String>,
    pub signal_percent: Option<u8>,
    pub rssi_dbm: Option<i32>,
    pub channel: Option<u32>,
    pub frequency_mhz: Option<u64>,
    /// `2.4ghz`, `5ghz`, `6ghz`, or `unknown`.
    pub band: Option<String>,
    pub security: Option<String>,
    /// Link quality when the OS exposes it (0-100).
    pub link_quality: Option<u8>,
}

/// Result of a Wi-Fi scan (tool `wifi_scan`).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct WifiScan {
    /// `ok`, `limited`, or `unavailable`.
    pub status: String,
    pub timestamp: String,
    pub adapter_id: Option<String>,
    /// Sorted by signal strength (strongest first) for determinism.
    pub networks: Vec<WifiNetwork>,
    /// True when the result was truncated to stay within limits.
    pub truncated: bool,
    pub unavailable: Vec<UnavailableReading>,
}

// ---------------------------------------------------------------------------
// Network diagnosis
// ---------------------------------------------------------------------------

/// A bounded composite network snapshot (tool `network_snapshot`).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct NetworkSnapshot {
    /// `ok`, `limited`, or `unavailable`.
    pub status: String,
    pub timestamp: String,
    pub duration_ms: u64,
    pub interfaces: Vec<crate::models::network::NetworkInterfaceInfo>,
    /// Wi-Fi adapter status; empty when the adapter list has no Wi-Fi.
    pub wifi: Vec<WifiAdapterStatus>,
    /// TCP connections, bounded by the configured limit.
    pub connections: Vec<crate::models::network::ConnectionInfo>,
    /// Listening ports, bounded by the configured limit.
    pub listening_ports: Vec<crate::models::network::PortInfo>,
    /// `full` or `limited`.
    pub completeness: String,
    pub unavailable: Vec<UnavailableReading>,
}

/// Connectivity health of one interface, gathered from up to three providers.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct NetworkDiagnosticInterface {
    pub description: String,
    pub is_wifi: bool,
    pub is_up: bool,
    pub gateway: Option<String>,
    /// Current signal percent (Wi-Fi only).
    pub signal_percent: Option<u8>,
    /// RSSI in dBm (Wi-Fi only).
    pub rssi_dbm: Option<i32>,
    pub link_speed_mbps: Option<f64>,
    /// Packet loss to the gateway during the probe window, when measured.
    pub packet_loss_percent: Option<f64>,
    /// Round-trip latency to the gateway during the probe window, when measured.
    pub gateway_latency_ms: Option<f64>,
}

/// One structured finding from a network diagnosis.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct NetworkFinding {
    /// Stable id, e.g. `wifi-weak-signal-<adapter>`.
    pub id: String,
    pub title: String,
    /// `info`, `low`, `medium`, `high`, or `critical`.
    pub severity: String,
    /// `confirmed`, `observed`, `likely`, `possible`, or `unknown`.
    pub confidence: String,
    /// Observations backing this finding.
    pub evidence: Vec<EvidencePoint>,
    pub detail: String,
    /// Observations that point away from this finding, when applicable.
    pub contradicting: Vec<String>,
}

/// Network diagnosis report (tool `network_diagnose`).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct NetworkDiagnosis {
    /// `ok`, `issues_detected`, `limited`, or `unavailable`.
    pub status: String,
    pub timestamp: String,
    pub duration_ms: u64,
    /// One-line summary for an agent.
    pub summary: String,
    pub interfaces: Vec<NetworkDiagnosticInterface>,
    pub findings: Vec<NetworkFinding>,
    /// `full` or `limited`.
    pub completeness: String,
    pub unavailable: Vec<UnavailableReading>,
    /// External-connectivity cross-check (a DNS resolution through the
    /// default resolver). `ok` = a well-known host resolved, `failed` = the
    /// resolver returned an error, `unconfirmed` = the check could not finish
    /// inside the probe budget, `not_probed` = no up gateway interface to
    /// justify the check. Used to interpret gateway ICMP loss: modern routers
    /// often filter ICMP, so loss alone is not an outage.
    pub external_connectivity: String,
}
