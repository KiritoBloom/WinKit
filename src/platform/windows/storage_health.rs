//! Storage health: NVMe S.M.A.R.T. health log via
//! `IOCTL_STORAGE_QUERY_PROPERTY` (storage adapter protocol-specific
//! property), plus disk identity from `Win32_DiskDrive`.
//!
//! Every disk also gets a health status from the OS storage stack
//! (`MSFT_PhysicalDisk` in `root\Microsoft\Windows\Storage`), which works
//! without elevation. It is used as a fallback when a richer NVMe log page
//! cannot be read or the drive is not NVMe. ATA S.M.A.R.T. pass-through is
//! deliberately not attempted by default: it is unreliable and usually
//! requires elevation.

use std::mem::size_of;

use windows_sys::Win32::Foundation::{
    CloseHandle, GENERIC_READ, GENERIC_WRITE, INVALID_HANDLE_VALUE,
};
use windows_sys::Win32::Storage::FileSystem::{
    CreateFileW, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING,
};
use windows_sys::Win32::Storage::Nvme::{NVME_HEALTH_INFO_LOG, NVME_LOG_PAGE_HEALTH_INFO};
use windows_sys::Win32::System::Ioctl::{
    NVMeDataTypeLogPage, PropertyStandardQuery, ProtocolTypeNvme,
    StorageAdapterProtocolSpecificProperty, IOCTL_STORAGE_QUERY_PROPERTY, STORAGE_PROPERTY_QUERY,
    STORAGE_PROTOCOL_SPECIFIC_DATA_EXT,
};
use windows_sys::Win32::System::IO::DeviceIoControl;

use crate::errors::WinkitError;
use crate::models::{
    DiskHealthReport, SensorAvailability, StorageHealthDevice, UnavailableReading,
};
use crate::platform::windows::hardware::HardwareOptions;
use crate::platform::windows::wmi::{WmiObject, WmiSession, WmiValue};

fn query_disk_drives() -> Result<Vec<WmiObject>, WinkitError> {
    let session = WmiSession::connect("root\\cimv2")?;
    session.query("SELECT DeviceID, Model, InterfaceType, Size FROM Win32_DiskDrive")
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

/// Health status for every physical disk from the OS storage stack. The
/// namespace and class are readable without elevation.
fn query_storage_stack_health() -> Result<Vec<WmiObject>, WinkitError> {
    let session = WmiSession::connect("root\\Microsoft\\Windows\\Storage")?;
    session.query("SELECT * FROM MSFT_PhysicalDisk")
}

/// Map `MSFT_PhysicalDisk.HealthStatus` to a report health status.
fn storage_health_status(health_status: u32) -> Option<&'static str> {
    match health_status {
        0 => Some("healthy"),
        1 => Some("warning"),
        2 => Some("critical"),
        _ => None,
    }
}

/// `MSFT_PhysicalDisk.MediaType`: 3 = HDD, 4 = SSD, 5 = SCM.
fn media_type_name(media_type: u32) -> Option<&'static str> {
    match media_type {
        3 => Some("hdd"),
        4 => Some("ssd"),
        5 => Some("scm"),
        _ => None,
    }
}

/// `MSFT_PhysicalDisk.BusType`, for the interfaces WinKit distinguishes.
fn bus_type_name(bus_type: u32) -> Option<&'static str> {
    match bus_type {
        11 => Some("sata"),
        17 => Some("nvme"),
        7 => Some("usb"),
        10 => Some("sas"),
        _ => None,
    }
}

/// Copy the diagnostics the storage stack exposes without elevation onto a
/// device report.
fn apply_stack_metadata(device: &mut StorageHealthDevice, st: &WmiObject) {
    device.media_type = st
        .get("MediaType")
        .and_then(WmiValue::as_u32)
        .and_then(media_type_name)
        .map(str::to_string);
    device.bus_type = st
        .get("BusType")
        .and_then(WmiValue::as_u32)
        .and_then(bus_type_name)
        .map(str::to_string);
    device.firmware_version = st.get_string("FirmwareVersion");
    device.serial_number = st.get_string("SerialNumber");
    device.physical_location = st.get_string("PhysicalLocation");
    device.spindle_speed_rpm = st.get_u32("SpindleSpeed").filter(|&v| v > 0);
}

/// The numeric index from a `PhysicalDriveN` device name.
fn physical_disk_number(device: &str) -> Option<u32> {
    device
        .to_ascii_lowercase()
        .strip_prefix("physicaldrive")
        .and_then(|rest| rest.parse::<u32>().ok())
}

/// Report one disk from the OS storage-stack health when present, otherwise
/// push an explicit unavailable reading.
fn push_storage_or_unavailable(
    devices: &mut Vec<StorageHealthDevice>,
    unavailable: &mut Vec<UnavailableReading>,
    device: String,
    model: Option<String>,
    interface: String,
    stack_health: Option<&WmiObject>,
    fallback_reason: &str,
) {
    let Some(st) = stack_health else {
        unavailable.push(UnavailableReading::new(
            &device,
            "health",
            SensorAvailability::Unavailable,
            fallback_reason,
        ));
        devices.push(StorageHealthDevice {
            device,
            model,
            interface,
            availability: SensorAvailability::Unavailable,
            reason: Some(fallback_reason.into()),
            ..Default::default()
        });
        return;
    };
    let model = st.get_string("Model").or(model);
    match st.get_u32("HealthStatus").and_then(storage_health_status) {
        Some(hs) => {
            let mut dev = StorageHealthDevice {
                device,
                model,
                interface,
                health_status: Some(hs.into()),
                availability: SensorAvailability::Available,
                ..Default::default()
            };
            apply_stack_metadata(&mut dev, st);
            devices.push(dev);
        }
        None => {
            let reason = "the OS storage stack exposed the disk but no health status";
            unavailable.push(UnavailableReading::new(
                &device,
                "health",
                SensorAvailability::Unavailable,
                reason,
            ));
            let mut dev = StorageHealthDevice {
                device,
                model,
                interface,
                availability: SensorAvailability::Unavailable,
                reason: Some(reason.into()),
                ..Default::default()
            };
            apply_stack_metadata(&mut dev, st);
            devices.push(dev);
        }
    }
}

/// Read the NVMe SMART health log page (0x02) for one physical drive.
fn nvme_health_log(device: &str) -> Option<NVME_HEALTH_INFO_LOG> {
    unsafe {
        let path = format!("\\\\.\\{}", device.trim_end_matches('\\'));
        let open = |access: u32| {
            CreateFileW(
                crate::utils::to_wide(&path).as_ptr(),
                access,
                FILE_SHARE_READ | FILE_SHARE_WRITE,
                std::ptr::null(),
                OPEN_EXISTING,
                0,
                std::ptr::null_mut(),
            )
        };
        let mut handle = open(GENERIC_READ | GENERIC_WRITE);
        if handle == INVALID_HANDLE_VALUE {
            // Retry with read-only access; some systems refuse write open.
            handle = open(GENERIC_READ);
        }
        if handle == INVALID_HANDLE_VALUE {
            // Non-elevated processes cannot open a physical drive for
            // read/write at all. A zero-desired-access handle still permits
            // `IOCTL_STORAGE_QUERY_PROPERTY`, so retry without access rights.
            handle = open(0);
        }
        if handle == INVALID_HANDLE_VALUE {
            return None;
        }

        let query_size = size_of::<STORAGE_PROPERTY_QUERY>();
        let protocol_size = size_of::<STORAGE_PROTOCOL_SPECIFIC_DATA_EXT>();
        let log_size = size_of::<NVME_HEALTH_INFO_LOG>();
        let mut buf = vec![0u8; query_size + protocol_size + log_size];

        let header = buf.as_mut_ptr() as *mut STORAGE_PROPERTY_QUERY;
        (*header).PropertyId = StorageAdapterProtocolSpecificProperty;
        (*header).QueryType = PropertyStandardQuery;
        (*header).AdditionalParameters = [0];

        let protocol = buf.as_mut_ptr().add(query_size) as *mut STORAGE_PROTOCOL_SPECIFIC_DATA_EXT;
        (*protocol).ProtocolType = ProtocolTypeNvme;
        (*protocol).DataType = NVMeDataTypeLogPage as u32;
        (*protocol).ProtocolDataValue = NVME_LOG_PAGE_HEALTH_INFO as u32;
        (*protocol).ProtocolDataSubValue = 0;
        (*protocol).ProtocolDataOffset = (query_size + protocol_size) as u32;
        (*protocol).ProtocolDataLength = log_size as u32;

        let mut returned = 0u32;
        let ok = DeviceIoControl(
            handle,
            IOCTL_STORAGE_QUERY_PROPERTY,
            buf.as_mut_ptr() as *mut std::ffi::c_void,
            (query_size + protocol_size) as u32,
            buf.as_mut_ptr() as *mut std::ffi::c_void,
            buf.len() as u32,
            &mut returned,
            std::ptr::null_mut(),
        );
        CloseHandle(handle);

        if ok == 0 {
            return None;
        }
        // FixedProtocolReturnData carries the storage stack's NTSTATUS;
        // anything but 0 is an NVMe layer failure.
        if (*protocol).FixedProtocolReturnData != 0 {
            return None;
        }
        let log_ptr = buf.as_ptr().add((*protocol).ProtocolDataOffset as usize)
            as *const NVME_HEALTH_INFO_LOG;
        let log: NVME_HEALTH_INFO_LOG = std::ptr::read(log_ptr);
        Some(log)
    }
}

fn u128_le(bytes: &[u8; 16]) -> u128 {
    let mut out = [0u8; 16];
    out.copy_from_slice(bytes);
    u128::from_le_bytes(out)
}

fn u64_from_u128(v: u128) -> Option<u64> {
    u64::try_from(v).ok()
}

fn critical_warnings(flags: u8) -> Vec<String> {
    let mut out = Vec::new();
    if flags & 0x01 != 0 {
        out.push("reliability_degraded".into());
    }
    if flags & 0x02 != 0 {
        out.push("temperature_above_threshold".into());
    }
    if flags & 0x04 != 0 {
        out.push("spare_capacity_low".into());
    }
    if flags & 0x08 != 0 {
        out.push("read_only_or_degraded_mode".into());
    }
    if flags & 0x10 != 0 {
        out.push("backup_device_failed".into());
    }
    out
}

fn warning_flags(log: &NVME_HEALTH_INFO_LOG) -> u8 {
    // The NVMe health log's CriticalWarning is a 1-byte bitfield; read it as
    // the union's raw byte.
    unsafe { log.CriticalWarning.AsUchar }
}

fn health_status_from(log: &NVME_HEALTH_INFO_LOG) -> Option<String> {
    let warnings = critical_warnings(warning_flags(log));
    if !warnings.is_empty() {
        return Some("critical".into());
    }
    let used = log.PercentageUsed;
    let spare = log.AvailableSpare;
    if used >= 100 || spare <= log.AvailableSpareThreshold {
        return Some("warning".into());
    }
    if used >= 80 {
        return Some("warning".into());
    }
    Some("healthy".into())
}

/// NVMe health for one device; `None` when the log cannot be read.
fn nvme_health(device: &str) -> Option<StorageHealthDevice> {
    let log = nvme_health_log(device)?;
    let temp_k = u16::from_le_bytes([log.Temperature[0], log.Temperature[1]]);
    Some(StorageHealthDevice {
        device: device.to_string(),
        model: None,
        interface: "nvme".into(),
        health_status: health_status_from(&log),
        temperature_c: if temp_k > 0 {
            Some(temp_k as f64 - 273.15)
        } else {
            None
        },
        critical_warning: critical_warnings(warning_flags(&log)),
        percentage_used: Some(log.PercentageUsed),
        available_spare: Some(log.AvailableSpare),
        available_spare_threshold: Some(log.AvailableSpareThreshold),
        media_errors: u64_from_u128(u128_le(&log.MediaErrors)),
        power_on_hours: u64_from_u128(u128_le(&log.PowerOnHours)),
        unsafe_shutdowns: u64_from_u128(u128_le(&log.UnsafeShutdowns)),
        data_units_read: u64_from_u128(u128_le(&log.DataUnitRead)),
        data_units_written: u64_from_u128(u128_le(&log.DataUnitWritten)),
        reallocated_sectors: None,
        media_type: None,
        bus_type: Some("nvme".into()),
        firmware_version: None,
        serial_number: None,
        physical_location: None,
        spindle_speed_rpm: None,
        availability: SensorAvailability::Available,
        reason: None,
    })
}

/// Storage health report for every physical disk.
pub fn disk_health(opts: &HardwareOptions) -> Result<DiskHealthReport, WinkitError> {
    let started = std::time::Instant::now();
    let mut unavailable = Vec::new();

    if !opts.sensors_enabled {
        unavailable.push(UnavailableReading::new(
            "all",
            "health",
            SensorAvailability::Unavailable,
            "hardware sensors are disabled in configuration ([hardware] sensors_enabled = false)",
        ));
        return Ok(DiskHealthReport {
            status: "unavailable".into(),
            timestamp: crate::utils::time::format_rfc3339(std::time::SystemTime::now()),
            duration_ms: started.elapsed().as_millis() as u64,
            devices: Vec::new(),
            completeness: "limited".into(),
            unavailable,
        });
    }

    let drives = match query_disk_drives() {
        Ok(d) => d,
        Err(e) => {
            unavailable.push(UnavailableReading::new(
                "storage",
                "identity",
                SensorAvailability::Unavailable,
                format!("Win32_DiskDrive query failed: {}", e.message),
            ));
            return Ok(DiskHealthReport {
                status: "unavailable".into(),
                timestamp: crate::utils::time::format_rfc3339(std::time::SystemTime::now()),
                duration_ms: started.elapsed().as_millis() as u64,
                devices: Vec::new(),
                completeness: "limited".into(),
                unavailable,
            });
        }
    };

    // Non-elevated health status from the OS storage stack. A failure here is
    // tolerated (systems without the storage provider may still expose NVMe
    // S.M.A.R.T. logs); each disk that neither source covers reports why.
    let storage_stack = query_storage_stack_health();
    if let Err(e) = &storage_stack {
        unavailable.push(UnavailableReading::new(
            "storage_stack",
            "health",
            SensorAvailability::Unavailable,
            format!("MSFT_PhysicalDisk query failed: {}", e.message),
        ));
    }
    let storage_stack = storage_stack.unwrap_or_default();

    let mut devices = Vec::new();
    for d in drives {
        let device = d
            .get_string("DeviceID")
            .unwrap_or_else(|| "PhysicalDrive?".into())
            .replace("\\\\.\\", "")
            .to_ascii_uppercase();
        let interface = d
            .get_string("InterfaceType")
            .map(|t| normalize_interface(&t))
            .unwrap_or_else(|| "unknown".into());

        // The storage stack numbers physical disks the same way as
        // `\\.\PHYSICALDRIVEn`, so match on the trailing index.
        let stack_health = physical_disk_number(&device).and_then(|n| {
            storage_stack
                .iter()
                .find(|p| p.get_u32("DeviceId") == Some(n))
        });

        if interface == "nvme" {
            match nvme_health(&device) {
                Some(mut h) => {
                    h.model = d.get_string("Model");
                    if let Some(st) = stack_health {
                        apply_stack_metadata(&mut h, st);
                    }
                    devices.push(h);
                }
                None => push_storage_or_unavailable(
                    &mut devices,
                    &mut unavailable,
                    device.clone(),
                    d.get_string("Model"),
                    interface.clone(),
                    stack_health,
                    "NVMe SMART health log could not be read (the drive may be busy, \
                     or the driver rejects the query)",
                ),
            }
        } else {
            let reason = if opts.ata_smart_enabled {
                "ATA S.M.A.R.T. pass-through is not implemented; the storage stack \
                 exposed no health for this disk"
                    .to_string()
            } else {
                "ATA S.M.A.R.T. pass-through is disabled by default ([hardware] \
                 ata_smart_enabled = false); the storage stack exposed no health for \
                 this disk"
                    .to_string()
            };
            push_storage_or_unavailable(
                &mut devices,
                &mut unavailable,
                device.clone(),
                d.get_string("Model"),
                interface.clone(),
                stack_health,
                &reason,
            );
        }
    }

    let any_critical = devices
        .iter()
        .any(|d| d.health_status.as_deref() == Some("critical"));
    let any_warning = devices
        .iter()
        .any(|d| d.health_status.as_deref() == Some("warning"));
    let any_unknown = devices.iter().any(|d| d.health_status.is_none());

    // `full` only when every device carries real S.M.A.R.T.-derived
    // attributes. A device whose health came from the OS storage stack
    // (`MSFT_PhysicalDisk`) has a status but no attribute data, so the
    // report is `limited` — it must not claim the S.M.A.R.T. completeness
    // it does not have.
    let smart_complete = !devices.is_empty()
        && devices.iter().all(|d| {
            d.percentage_used.is_some()
                || d.available_spare.is_some()
                || d.media_errors.is_some()
                || d.power_on_hours.is_some()
                || d.unsafe_shutdowns.is_some()
                || d.data_units_read.is_some()
                || d.data_units_written.is_some()
                || d.reallocated_sectors.is_some()
                || d.temperature_c.is_some()
        });
    let completeness = if unavailable.is_empty() && (devices.is_empty() || smart_complete) {
        "full"
    } else {
        "limited"
    };

    Ok(DiskHealthReport {
        status: if any_critical {
            "critical".into()
        } else if any_warning {
            "warning".into()
        } else if any_unknown && !devices.is_empty() {
            "unknown".into()
        } else if devices.is_empty() {
            "not_applicable".into()
        } else {
            "healthy".into()
        },
        timestamp: crate::utils::time::format_rfc3339(std::time::SystemTime::now()),
        duration_ms: started.elapsed().as_millis() as u64,
        devices,
        completeness: completeness.into(),
        unavailable,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn storage_health_status_maps_microsoft_enum() {
        assert_eq!(storage_health_status(0), Some("healthy"));
        assert_eq!(storage_health_status(1), Some("warning"));
        assert_eq!(storage_health_status(2), Some("critical"));
        assert_eq!(storage_health_status(5), None);
    }

    #[test]
    fn media_type_maps_storage_stack_values() {
        assert_eq!(media_type_name(3), Some("hdd"));
        assert_eq!(media_type_name(4), Some("ssd"));
        assert_eq!(media_type_name(5), Some("scm"));
        assert_eq!(media_type_name(0), None);
    }

    #[test]
    fn bus_type_maps_interfaces_winkit_distinguishes() {
        assert_eq!(bus_type_name(11), Some("sata"));
        assert_eq!(bus_type_name(17), Some("nvme"));
        assert_eq!(bus_type_name(7), Some("usb"));
        assert_eq!(bus_type_name(0), None);
    }

    #[test]
    fn physical_disk_number_extracts_trailing_index() {
        assert_eq!(physical_disk_number("PHYSICALDRIVE0"), Some(0));
        assert_eq!(physical_disk_number("PhysicalDrive12"), Some(12));
        assert_eq!(physical_disk_number("PHYSICALDRIVE"), None);
        assert_eq!(physical_disk_number("USBSTOR\\DISK&VEN"), None);
    }
}

/// Live Windows regression tests (opt-in): `WINKIT_LIVE_WINDOWS=1 cargo test
/// --features live-windows`. Guards the OS storage-stack health fallback used
/// by `disk_health`: the `MSFT_PhysicalDisk` query must return instances with
/// readable `DeviceId` values without elevation.
#[cfg(all(test, feature = "live-windows"))]
mod live_windows {
    use super::*;

    fn live_enabled() -> bool {
        std::env::var("WINKIT_LIVE_WINDOWS")
            .map(|v| v == "1")
            .unwrap_or(false)
    }

    fn live_skip(reason: &str) {
        eprintln!("SKIP: {reason}");
    }

    #[test]
    fn storage_stack_health_query_returns_readable_devices() {
        if !live_enabled() {
            live_skip("live diagnostic harness not enabled; run with WINKIT_LIVE_WINDOWS=1");
            return;
        }
        let disks = query_storage_stack_health().expect("MSFT_PhysicalDisk query succeeds");
        assert!(!disks.is_empty(), "expected at least one physical disk");
        for d in &disks {
            assert!(
                d.get_u32("DeviceId").is_some(),
                "each MSFT_PhysicalDisk instance must expose a readable DeviceId"
            );
        }
    }
}
