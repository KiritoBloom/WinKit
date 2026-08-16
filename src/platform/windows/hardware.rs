//! Hardware snapshot and thermal telemetry.
//!
//! Providers used, in order of trust:
//! - `MSAcpi_ThermalZoneTemperature` (WMI, root\WMI): ACPI thermal zones.
//! - `Win32_Processor` / `Win32_VideoController` / `Win32_PhysicalMemory` /
//!   `Win32_DiskDrive` (WMI, root\cimv2): static hardware identity.
//! - PDH `% Processor Performance`: current CPU clock relative to base.
//!
//! Honesty rules: a reading that cannot be produced by a documented path is
//! returned as explicitly unavailable with a reason. GPU temperature has no
//! documented read path on Windows without a vendor SDK, so it is always
//! reported unavailable — never inferred, never "healthy".

use crate::errors::{ErrorKind, WinkitError};
use crate::log_warn;
use crate::models::*;
use crate::platform::windows::pdh;
use crate::platform::windows::system::sample_cpu_busy_percent;
use crate::platform::windows::wmi::{WmiObject, WmiSession, WmiValue};

/// Documented heuristic thresholds for the thermal summary. The
/// configurable `DiagnosticsConfig` thresholds govern `system_diagnose`;
/// these constants govern the standalone thermal snapshot and are documented
/// in `docs/diagnostics.md`.
const HIGH_CPU_TEMP_C: f64 = 85.0;
const THROTTLE_CPU_TEMP_C: f64 = 95.0;
const CPU_FREQ_REDUCTION_RATIO: f64 = 0.85;

/// Options that control hardware collection, derived from `[hardware]`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HardwareOptions {
    pub sensors_enabled: bool,
    pub wifi_scan_enabled: bool,
    pub ata_smart_enabled: bool,
}

impl Default for HardwareOptions {
    fn default() -> Self {
        Self {
            sensors_enabled: true,
            wifi_scan_enabled: false,
            ata_smart_enabled: false,
        }
    }
}

impl HardwareOptions {
    pub fn from_config(c: &crate::config::schema::HardwareConfig) -> Self {
        Self {
            sensors_enabled: c.sensors_enabled,
            wifi_scan_enabled: c.wifi_scan_enabled,
            ata_smart_enabled: c.ata_smart_enabled,
        }
    }
}

fn now() -> String {
    crate::utils::time::format_rfc3339(std::time::SystemTime::now())
}

/// Query `root\cimv2` for a class; returns an error only when the namespace
/// itself is unreachable, otherwise an empty vector.
fn query_cimv2(wql: &str) -> Result<Vec<WmiObject>, WinkitError> {
    match WmiSession::connect("root\\cimv2") {
        Ok(session) => session.query(wql),
        Err(e) => Err(WinkitError::new(
            ErrorKind::WindowsApiError,
            format!("root\\cimv2 unavailable: {}", e.message),
        )),
    }
}

// ---------------------------------------------------------------------------
// Thermal snapshot
// ---------------------------------------------------------------------------

/// `WBEM_E_ACCESS_DENIED` (0x80041003) and friendly variants of it.
fn is_access_denied(e: &WinkitError) -> bool {
    e.message.contains("0x80041003") || e.message.to_ascii_lowercase().contains("access denied")
}

/// Collect thermal-zone sensors. Returns `(sensors, unavailable, warnings)`.
fn collect_thermal_sensors() -> (Vec<SensorReading>, Vec<UnavailableReading>, Vec<String>) {
    let mut sensors = Vec::new();
    let mut unavailable = Vec::new();
    let mut warnings = Vec::new();

    let zones = match WmiSession::connect("root\\WMI") {
        Ok(session) => match session.query("SELECT * FROM MSAcpi_ThermalZoneTemperature") {
            Ok(z) => z,
            Err(e) => {
                let denied = is_access_denied(&e);
                let reason = if denied {
                    "the ACPI thermal-zone WMI class (MSAcpi_ThermalZoneTemperature) \
                     requires elevation on this host; WMI denied the query"
                        .to_string()
                } else {
                    format!("WMI query failed: {}", e.message)
                };
                unavailable.push(UnavailableReading::new(
                    "acpi_thermal_zones",
                    "temperature",
                    if denied {
                        SensorAvailability::PermissionDenied
                    } else {
                        SensorAvailability::Unavailable
                    },
                    reason,
                ));
                warnings.push(if denied {
                    "ACPI thermal zones are elevation-gated on this host; run elevated \
                     to read them"
                        .to_string()
                } else {
                    "no ACPI thermal zones were readable via WMI".to_string()
                });
                return (sensors, unavailable, warnings);
            }
        },
        Err(e) => {
            unavailable.push(UnavailableReading::new(
                "acpi_thermal_zones",
                "temperature",
                SensorAvailability::Unavailable,
                format!("cannot reach root\\WMI: {}", e.message),
            ));
            warnings.push("no ACPI thermal zones were readable via WMI".to_string());
            return (sensors, unavailable, warnings);
        }
    };

    if zones.is_empty() {
        unavailable.push(UnavailableReading::new(
            "acpi_thermal_zones",
            "temperature",
            SensorAvailability::NotPresent,
            "the ACPI firmware exposes no thermal zone instances (MSAcpi_ThermalZoneTemperature)",
        ));
        warnings.push("this machine exposes no ACPI thermal zones".to_string());
        return (sensors, unavailable, warnings);
    }

    for (i, zone) in zones.iter().enumerate() {
        let instance = zone.instance_name();
        let label = instance
            .as_deref()
            .unwrap_or(&format!("zone-{i}"))
            .to_string();
        let id = format!("thermal_zone-{label}");
        // CurrentTemperature is in tenths of Kelvin.
        match zone.get("CurrentTemperature").and_then(WmiValue::as_u32) {
            Some(raw) if raw != 0 && raw != u32::MAX => {
                let celsius = raw as f64 / 10.0 - 273.15;
                let is_cpu = instance.as_deref().is_some_and(|n| {
                    let n = n.to_ascii_lowercase();
                    n.contains("cpu") || n.contains("proc") || n.contains("package")
                });
                let class = if is_cpu {
                    SensorClass::CpuPackage
                } else {
                    SensorClass::ThermalZone
                };
                let mut reading = SensorReading::available(
                    id.clone(),
                    format!("Thermal zone {label}"),
                    class,
                    SensorKind::Temperature,
                    label.clone(),
                    celsius,
                    "temperature_c",
                    SensorSource::ThermalZone,
                    SensorQuality::High,
                    None,
                    None,
                );
                reading.status = if celsius >= THROTTLE_CPU_TEMP_C {
                    SensorStatus::Critical
                } else if celsius >= HIGH_CPU_TEMP_C {
                    SensorStatus::Warning
                } else {
                    SensorStatus::Ok
                };
                sensors.push(reading);
            }
            _ => {
                unavailable.push(UnavailableReading::new(
                    label,
                    "temperature",
                    SensorAvailability::Unavailable,
                    "thermal zone reported no current temperature (CurrentTemperature 0 or max)",
                ));
            }
        }
    }
    (sensors, unavailable, warnings)
}

/// CPU package temperature from the ACPI thermal zones when one is clearly a
/// CPU zone; otherwise `None`.
fn cpu_package_temperature(sensors: &[SensorReading]) -> Option<f64> {
    sensors
        .iter()
        .filter(|s| s.class == SensorClass::CpuPackage && s.value.is_some())
        .map(|s| s.value.unwrap())
        .max_by(|a, b| a.total_cmp(b))
}

fn gpu_temperature_unavailable() -> UnavailableReading {
    UnavailableReading::new(
        "gpu",
        "temperature",
        SensorAvailability::Unsupported,
        "no documented Windows API exposes GPU temperature without a vendor SDK \
         (NVML/ADL); GPU temperature is not reported",
    )
}

/// Thermal snapshot of the machine.
pub fn thermal_snapshot(opts: &HardwareOptions) -> Result<ThermalSnapshot, WinkitError> {
    let started = std::time::Instant::now();
    let mut warnings = Vec::new();
    let mut unavailable = Vec::new();

    if !opts.sensors_enabled {
        unavailable.push(UnavailableReading::new(
            "all",
            "temperature",
            SensorAvailability::Unavailable,
            "hardware sensors are disabled in configuration ([hardware] sensors_enabled = false)",
        ));
        return Ok(ThermalSnapshot {
            status: "no_sensors".into(),
            timestamp: now(),
            duration_ms: started.elapsed().as_millis() as u64,
            sensors: Vec::new(),
            thermal_state: ThermalStateSummary::default(),
            completeness: "limited".into(),
            unavailable,
            warnings,
        });
    }

    let (mut sensors, zone_unavailable, zone_warnings) = collect_thermal_sensors();
    let zone_has_unavailable = !zone_unavailable.is_empty();
    let zones_permission_denied = zone_unavailable
        .iter()
        .any(|u| u.availability == SensorAvailability::PermissionDenied);
    unavailable.extend(zone_unavailable);
    warnings.extend(zone_warnings);

    // CPU package temperature: prefer a clearly-labeled CPU zone.
    let mut cpu_temp = cpu_package_temperature(&sensors);
    if cpu_temp.is_none() {
        // Fall back to the first zone (many laptops put the CPU in zone 0).
        cpu_temp = sensors
            .iter()
            .find(|s| s.value.is_some())
            .and_then(|s| s.value);
    }
    if cpu_temp.is_none() {
        if zones_permission_denied {
            unavailable.push(UnavailableReading::new(
                "cpu_package",
                "temperature",
                SensorAvailability::PermissionDenied,
                "the only native CPU temperature source (ACPI thermal zones) is \
                 elevation-gated on this host; run elevated to read it",
            ));
        } else {
            unavailable.push(UnavailableReading::new(
                "cpu_package",
                "temperature",
                SensorAvailability::Unsupported,
                "no documented CPU package temperature source is available on this machine \
                 (no vendor SDK, no ACPI CPU zone)",
            ));
        }
    }

    unavailable.push(gpu_temperature_unavailable());

    // Current CPU frequency, when PDH can read it.
    let base = cpu_base_clock_mhz();
    let current_freq = base.and_then(pdh::current_cpu_frequency_mhz);
    let mut evidence = Vec::new();
    let mut limitations = Vec::new();
    let mut cpu_frequency_reduced: Option<bool> = None;

    if let (Some(base), Some(current)) = (base, current_freq) {
        evidence.push(EvidencePoint {
            metric: "cpu_frequency_mhz".into(),
            value: format!("{current:.0} MHz (base {base:.0} MHz)"),
            detail: r"\Processor Information(_Total)\% Processor Performance scaled by base clock"
                .into(),
        });
        let ratio = current / base;
        if ratio < CPU_FREQ_REDUCTION_RATIO {
            cpu_frequency_reduced = Some(true);
        } else if ratio > 1.05 {
            cpu_frequency_reduced = Some(false);
        }
    } else {
        limitations
            .push("current CPU frequency could not be read (PDH counter unavailable)".to_string());
    }

    if let Some(t) = cpu_temp {
        evidence.push(EvidencePoint {
            metric: "cpu_temperature_c".into(),
            value: format!("{t:.1} C"),
            detail: "ACPI thermal zone temperature".into(),
        });
        let mut freq_sensor = SensorReading::available(
            "cpu_frequency",
            "CPU current frequency",
            SensorClass::CpuPackage,
            SensorKind::ClockRate,
            "cpu_package",
            current_freq.unwrap_or_default(),
            "mhz",
            SensorSource::PerformanceCounter,
            SensorQuality::Medium,
            None,
            base,
        );
        freq_sensor.availability = if current_freq.is_some() {
            SensorAvailability::Available
        } else {
            SensorAvailability::Unavailable
        };
        freq_sensor.reason = current_freq
            .is_none()
            .then(|| "PDH frequency counter unavailable".to_string());
        sensors.push(freq_sensor);
    }

    // Interpret the summary from the measured facts.
    let mut state = ThermalStateSummary {
        cpu_frequency_reduced,
        ..ThermalStateSummary::default()
    };
    match cpu_temp {
        Some(t) if t >= THROTTLE_CPU_TEMP_C => {
            state.cpu_throttling = "likely".into();
            state.cpu_thermal_pressure = "high".into();
        }
        Some(t) if t >= HIGH_CPU_TEMP_C => {
            state.cpu_throttling = "not_observed".into();
            state.cpu_thermal_pressure = "elevated".into();
        }
        Some(_) => {
            state.cpu_throttling = "not_observed".into();
            state.cpu_thermal_pressure = "low".into();
        }
        None => {
            state.cpu_throttling = "unknown".into();
            state.cpu_thermal_pressure = "unknown".into();
            limitations.push("no CPU temperature sensor, so throttling cannot be assessed".into());
        }
    }
    state.gpu_throttling = "unknown".into();
    state.gpu_thermal_pressure = "unknown".into();
    limitations.push(
        "GPU temperature is not readable without a vendor SDK; GPU throttling is unknown".into(),
    );
    if let Some(true) = cpu_frequency_reduced {
        state.cpu_throttling = "likely".into();
        evidence.push(EvidencePoint {
            metric: "cpu_frequency_reduced".into(),
            value: "true".into(),
            detail: "CPU frequency is well below base clock while the machine is active".into(),
        });
    }
    state.evidence = evidence;
    state.limitations = limitations.clone();

    sensors.sort_by(|a, b| a.sensor_id.cmp(&b.sensor_id));

    let status = if sensors.is_empty() {
        "no_sensors".to_string()
    } else if sensors
        .iter()
        .any(|s| s.status == SensorStatus::Critical || s.status == SensorStatus::Warning)
    {
        "degraded".to_string()
    } else {
        "ok".to_string()
    };

    Ok(ThermalSnapshot {
        status,
        timestamp: now(),
        duration_ms: started.elapsed().as_millis() as u64,
        sensors,
        thermal_state: state,
        completeness: if zone_has_unavailable {
            "limited".into()
        } else {
            "full".into()
        },
        unavailable,
        warnings,
    })
}

// ---------------------------------------------------------------------------
// Hardware snapshot
// ---------------------------------------------------------------------------

/// Base clock (MaxClockSpeed) from `Win32_Processor`.
pub fn cpu_base_clock_mhz() -> Option<f64> {
    match query_cimv2("SELECT MaxClockSpeed FROM Win32_Processor") {
        Ok(objs) => objs
            .iter()
            .find_map(|o| o.get_u32("MaxClockSpeed"))
            .filter(|&v| v > 0)
            .map(|v| v as f64),
        Err(e) => {
            log_warn!("cpu base clock query failed: {}", e.message);
            None
        }
    }
}

fn collect_cpu(opts: &HardwareOptions) -> (CpuHardwareInfo, Vec<UnavailableReading>) {
    let mut info = CpuHardwareInfo::default();
    let mut unavailable = Vec::new();
    if !opts.sensors_enabled {
        unavailable.push(UnavailableReading::new(
            "cpu",
            "identity",
            SensorAvailability::Unavailable,
            "hardware sensors are disabled in configuration",
        ));
        return (info, unavailable);
    }

    match query_cimv2(
        "SELECT Name, Manufacturer, Family, Stepping, NumberOfCores, \
         NumberOfLogicalProcessors, MaxClockSpeed, CurrentClockSpeed FROM Win32_Processor",
    ) {
        Ok(objs) => {
            if let Some(cpu) = objs.first() {
                info.name = cpu.get_string("Name");
                info.vendor = cpu.get_string("Manufacturer");
                info.family = cpu.get_u32("Family");
                info.model = cpu.get_u32("Model");
                info.stepping = cpu.get_u32("Stepping");
                info.cores = cpu.get_u32("NumberOfCores");
                info.logical_processors = cpu.get_u32("NumberOfLogicalProcessors");
                info.base_clock_mhz = cpu
                    .get_u32("MaxClockSpeed")
                    .filter(|&v| v > 0)
                    .map(|v| v as f64);
                let current = info.base_clock_mhz.and_then(pdh::current_cpu_frequency_mhz);
                info.current_clock_mhz = current;
            } else {
                unavailable.push(UnavailableReading::new(
                    "cpu",
                    "identity",
                    SensorAvailability::Unavailable,
                    "Win32_Processor returned no instances",
                ));
            }
        }
        Err(e) => {
            unavailable.push(UnavailableReading::new(
                "cpu",
                "identity",
                SensorAvailability::Unavailable,
                format!("Win32_Processor query failed: {}", e.message),
            ));
        }
    }

    // Package temperature (best-effort, from thermal zones).
    let (sensors, _, _) = collect_thermal_sensors();
    if let Some(t) = cpu_package_temperature(&sensors) {
        info.package_temperature_c = Some(t);
        info.temperature_source = Some("acpi_thermal_zone".into());
    } else if opts.sensors_enabled {
        info.temperature_source = Some("none".into());
    }

    // Utilization: fresh sample over a short window.
    match sample_cpu_busy_percent(200) {
        Ok(Some(pct)) if pct.is_finite() => info.utilization_percent = Some(pct),
        Ok(_) | Err(_) => {}
    }

    (info, unavailable)
}

fn gpu_vendor(pnp_device_id: &str) -> &'static str {
    let upper = pnp_device_id.to_ascii_uppercase();
    if upper.contains("VEN_10DE") || upper.contains("VEN_10DE&") {
        "nvidia"
    } else if upper.contains("VEN_1002") || upper.contains("VEN_1022") {
        "amd"
    } else if upper.contains("VEN_8086") {
        "intel"
    } else {
        "unknown"
    }
}

fn collect_gpus(opts: &HardwareOptions) -> (Vec<GpuHardwareInfo>, Vec<UnavailableReading>) {
    let mut unavailable = Vec::new();
    let mut gpus = Vec::new();
    if !opts.sensors_enabled {
        unavailable.push(UnavailableReading::new(
            "gpu",
            "identity",
            SensorAvailability::Unavailable,
            "hardware sensors are disabled in configuration",
        ));
        return (gpus, unavailable);
    }
    match query_cimv2(
        "SELECT Name, DriverVersion, AdapterRAM, PNPDeviceID FROM Win32_VideoController",
    ) {
        Ok(objs) => {
            for g in objs {
                let pnp = g.get_string("PNPDeviceID").unwrap_or_default();
                gpus.push(GpuHardwareInfo {
                    name: g.get_string("Name"),
                    vendor: gpu_vendor(&pnp).to_string(),
                    driver_version: g.get_string("DriverVersion"),
                    video_memory_bytes: g
                        .get_u32("AdapterRAM")
                        .filter(|&v| v > 0)
                        .map(|v| v as u64),
                    temperature_available: false,
                    temperature_c: None,
                    temperature_reason: Some(
                        "no documented GPU temperature read path on Windows without a vendor SDK"
                            .into(),
                    ),
                });
            }
            if gpus.is_empty() {
                unavailable.push(UnavailableReading::new(
                    "gpu",
                    "identity",
                    SensorAvailability::NotPresent,
                    "Win32_VideoController returned no adapters",
                ));
            }
        }
        Err(e) => {
            unavailable.push(UnavailableReading::new(
                "gpu",
                "identity",
                SensorAvailability::Unavailable,
                format!("Win32_VideoController query failed: {}", e.message),
            ));
        }
    }
    (gpus, unavailable)
}

fn collect_memory(opts: &HardwareOptions) -> (MemoryHardwareInfo, Vec<UnavailableReading>) {
    let mut info = MemoryHardwareInfo::default();
    let mut unavailable = Vec::new();
    if !opts.sensors_enabled {
        unavailable.push(UnavailableReading::new(
            "memory",
            "identity",
            SensorAvailability::Unavailable,
            "hardware sensors are disabled in configuration",
        ));
        return (info, unavailable);
    }
    match query_cimv2("SELECT Capacity FROM Win32_PhysicalMemory") {
        Ok(objs) => {
            info.module_count = Some(objs.len() as u32);
            let total: u64 = objs
                .iter()
                .filter_map(|o| o.get("Capacity").and_then(WmiValue::as_u64))
                .sum();
            if total > 0 {
                info.total_bytes = Some(total);
            }
        }
        Err(e) => {
            unavailable.push(UnavailableReading::new(
                "memory",
                "identity",
                SensorAvailability::Unavailable,
                format!("Win32_PhysicalMemory query failed: {}", e.message),
            ));
        }
    }
    (info, unavailable)
}

fn normalize_interface(t: &str) -> String {
    let up = t.to_ascii_uppercase();
    if up.contains("NVME") {
        "nvme".into()
    } else if up.contains("USB") {
        "usb".into()
    } else if up.contains("SCSI") || up.contains("IDE") || up.contains("ATA") || up.contains("SATA")
    {
        "sata".into()
    } else {
        "unknown".into()
    }
}

/// The index of the physical disk that hosts the system volume, via
/// `IOCTL_STORAGE_GET_DEVICE_NUMBER` on the system drive root.
/// Disk index of the physical drive backing the system volume. Uses the
/// WMI association classes (`Win32_LogicalDiskToPartition` +
/// `Win32_DiskDriveToDiskPartition`) so no elevation is required — opening
/// `\\.\C:` directly with `DeviceIoControl` fails with access denied for
/// non-admin users.
fn system_disk_index() -> Option<u32> {
    let system = std::env::var("SystemDrive").unwrap_or_else(|_| "C:".into());
    let want = format!("DeviceID=\"{}\"", system.trim_end_matches('\\'));

    // Map the system volume to its disk partition (Disk #0, Partition #1 …).
    let partition = query_cimv2("SELECT Antecedent, Dependent FROM Win32_LogicalDiskToPartition")
        .ok()?
        .into_iter()
        .find(|o| {
            o.get_string("Dependent")
                .is_some_and(|dep| dep.contains(&want))
        })?
        .get_string("Antecedent")?;
    let partition = partition
        .rsplit(':')
        .next()
        .map(str::trim)
        .unwrap_or_default();

    // Map the partition to its physical drive and read its index.
    let drive = query_cimv2("SELECT Antecedent, Dependent FROM Win32_DiskDriveToDiskPartition")
        .ok()?
        .into_iter()
        .find(|o| {
            o.get_string("Dependent")
                .is_some_and(|dep| dep.contains(partition))
        })?
        .get_string("Antecedent")?;
    disk_index_from_antecedent(&drive)
}

/// Extract the device number from a `Win32_DiskDrive` antecedent path such
/// as `\\DESKTOP\root\cimv2:Win32_DiskDrive.DeviceID="\\.\PHYSICALDRIVE0"`.
fn disk_index_from_antecedent(path: &str) -> Option<u32> {
    let index = path
        .to_ascii_lowercase()
        .rsplit("physicaldrive")
        .next()?
        .trim_matches(|c: char| !c.is_ascii_digit())
        .parse::<u32>()
        .ok()?;
    Some(index)
}

fn collect_storage_devices(
    opts: &HardwareOptions,
) -> (Vec<StorageDeviceInfo>, Vec<UnavailableReading>) {
    let mut devices = Vec::new();
    let mut unavailable = Vec::new();
    let system_index = if opts.sensors_enabled {
        system_disk_index()
    } else {
        None
    };
    if !opts.sensors_enabled {
        unavailable.push(UnavailableReading::new(
            "storage",
            "identity",
            SensorAvailability::Unavailable,
            "hardware sensors are disabled in configuration",
        ));
        return (devices, unavailable);
    }
    match query_cimv2("SELECT DeviceID, Model, InterfaceType, Size FROM Win32_DiskDrive") {
        Ok(objs) => {
            for d in objs {
                let device = d
                    .get_string("DeviceID")
                    .unwrap_or_else(|| "PhysicalDrive?".into());
                let index = device
                    .to_ascii_lowercase()
                    .replace("\\\\.\\physicaldrive", "")
                    .parse::<u32>()
                    .ok();
                devices.push(StorageDeviceInfo {
                    device: device.replace("\\\\.\\", "").to_ascii_uppercase(),
                    model: d.get_string("Model"),
                    interface: d
                        .get_string("InterfaceType")
                        .map(|t| normalize_interface(&t))
                        .unwrap_or_else(|| "unknown".into()),
                    capacity_bytes: d.get("Size").and_then(WmiValue::as_u64),
                    is_system: index.zip(system_index).is_some_and(|(i, s)| i == s),
                });
            }
            if devices.is_empty() {
                unavailable.push(UnavailableReading::new(
                    "storage",
                    "identity",
                    SensorAvailability::Unavailable,
                    "Win32_DiskDrive returned no instances",
                ));
            }
        }
        Err(e) => {
            unavailable.push(UnavailableReading::new(
                "storage",
                "identity",
                SensorAvailability::Unavailable,
                format!("Win32_DiskDrive query failed: {}", e.message),
            ));
        }
    }
    (devices, unavailable)
}

fn collect_network_adapters() -> Vec<NetworkAdapterInfo> {
    let mut out = Vec::new();
    match crate::platform::windows::network::list_network_interfaces() {
        Ok(ifaces) => {
            for i in ifaces {
                let desc_lower = i.description.to_ascii_lowercase();
                out.push(NetworkAdapterInfo {
                    index: i.index,
                    name: i.name,
                    description: i.description,
                    mac_address: i.mac_address,
                    is_wifi: desc_lower.contains("wi-fi")
                        || desc_lower.contains("wireless")
                        || desc_lower.contains("wlan"),
                    is_up: i.is_up,
                    ipv4_addresses: i.ipv4_addresses,
                    gateway: i.gateway,
                });
            }
        }
        Err(e) => log_warn!("network adapter enumeration failed: {}", e.message),
    }
    out
}

/// Complete bounded hardware snapshot.
pub fn hardware_snapshot(opts: &HardwareOptions) -> Result<HardwareSnapshot, WinkitError> {
    let started = std::time::Instant::now();
    let mut unavailable = Vec::new();

    if !opts.sensors_enabled {
        unavailable.push(UnavailableReading::new(
            "all",
            "hardware",
            SensorAvailability::Unavailable,
            "hardware sensors are disabled in configuration ([hardware] sensors_enabled = false)",
        ));
        return Ok(HardwareSnapshot {
            status: "limited".into(),
            timestamp: now(),
            duration_ms: started.elapsed().as_millis() as u64,
            cpu: CpuHardwareInfo::default(),
            gpus: Vec::new(),
            memory: MemoryHardwareInfo::default(),
            storage: Vec::new(),
            network_adapters: collect_network_adapters(),
            battery: None,
            power_state: PowerStateInfo::default(),
            sensors: Vec::new(),
            completeness: "limited".into(),
            unavailable,
        });
    }

    let (cpu, cpu_unavailable) = collect_cpu(opts);
    unavailable.extend(cpu_unavailable);
    let (gpus, gpu_unavailable) = collect_gpus(opts);
    unavailable.extend(gpu_unavailable);
    let (memory, memory_unavailable) = collect_memory(opts);
    unavailable.extend(memory_unavailable);
    let (storage, storage_unavailable) = collect_storage_devices(opts);
    unavailable.extend(storage_unavailable);

    let (thermal_sensors, thermal_unavailable, _) = collect_thermal_sensors();
    unavailable.extend(thermal_unavailable);

    let (power, power_unavailable) = crate::platform::windows::power::power_state();
    unavailable.extend(power_unavailable);
    let battery = crate::platform::windows::power::battery_present_summary();

    let battery_info =
        battery.map(
            |(present, percent, ac_online, charging, remaining)| BatteryInfo {
                present,
                percent,
                ac_online,
                charging: Some(charging),
                estimated_time_remaining_seconds: remaining,
            },
        );

    let mut sensors = thermal_sensors;
    if let Some(freq) = cpu.current_clock_mhz {
        let base = cpu.base_clock_mhz.unwrap_or(freq);
        let mut s = SensorReading::available(
            "cpu_frequency",
            "CPU current frequency",
            SensorClass::CpuPackage,
            SensorKind::ClockRate,
            "cpu_package",
            freq,
            "mhz",
            SensorSource::PerformanceCounter,
            SensorQuality::Medium,
            None,
            Some(base),
        );
        s.status = SensorStatus::Ok;
        sensors.push(s);
    }
    sensors.sort_by(|a, b| a.sensor_id.cmp(&b.sensor_id));

    Ok(HardwareSnapshot {
        status: if unavailable.is_empty() {
            "ok"
        } else {
            "limited"
        }
        .into(),
        timestamp: now(),
        duration_ms: started.elapsed().as_millis() as u64,
        cpu,
        gpus,
        memory,
        storage,
        network_adapters: collect_network_adapters(),
        battery: battery_info,
        power_state: power,
        sensors,
        completeness: if unavailable.is_empty() {
            "full"
        } else {
            "limited"
        }
        .into(),
        unavailable,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn disk_index_extracts_device_number_from_antecedent() {
        let path = r#"\\DESKTOP\root\cimv2:Win32_DiskDrive.DeviceID="\\.\PHYSICALDRIVE0""#;
        assert_eq!(disk_index_from_antecedent(path), Some(0));
        let multi = r#"\\HOST\root\cimv2:Win32_DiskDrive.DeviceID="\\.\PHYSICALDRIVE2""#;
        assert_eq!(disk_index_from_antecedent(multi), Some(2));
        assert_eq!(disk_index_from_antecedent("no drive here"), None);
    }
}
