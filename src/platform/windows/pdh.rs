//! Performance-counter (PDH) helpers for CPU frequency and disk activity.
//!
//! PDH counters need two samples separated by a sleep to produce rates.
//! Every query here is self-contained: open, sample, sleep, sample, read,
//! close. A counter that does not exist (`PDH_CSTATUS_NO_OBJECT`) is
//! reported as `None`/unavailable rather than an error, because Windows
//! hides some counters when the corresponding hardware is absent.
//!
//! `disk_activity` deliberately keeps *all* counters for *all* disks in one
//! PDH query so the two samples cost a single sleep, not one sleep per
//! counter per disk.

use windows_sys::Win32::System::Performance::{
    PdhAddEnglishCounterW, PdhCloseQuery, PdhCollectQueryData, PdhEnumObjectItemsW,
    PdhGetFormattedCounterValue, PdhMakeCounterPathW, PdhOpenQueryW, PDH_COUNTER_PATH_ELEMENTS_W,
    PDH_CSTATUS_NO_OBJECT, PDH_CSTATUS_VALID_DATA, PDH_FMT, PDH_FMT_COUNTERVALUE, PDH_FMT_DOUBLE,
    PDH_MORE_DATA, PERF_DETAIL_WIZARD,
};
use windows_sys::Win32::System::Threading::Sleep;

use crate::errors::WinkitError;
use crate::log_warn;
use crate::models::{DiskActivity, SensorAvailability, StorageActivity, UnavailableReading};

const PHYSICAL_DISK_OBJECT: &str = "PhysicalDisk";
const PROCESSOR_INFORMATION_OBJECT: &str = "Processor Information";
const THERMAL_ZONE_OBJECT: &str = "Thermal Zone Information";

/// Per-disk activity counters, in the order `DiskActivity` fields expect.
const DISK_COUNTERS: [&str; 6] = [
    "% Disk Time",
    "Current Disk Queue Length",
    "Disk Read Bytes/sec",
    "Disk Write Bytes/sec",
    "Disk Reads/sec",
    "Disk Writes/sec",
];

/// Build a counter path with `PdhMakeCounterPathW`, which handles instance
/// name escaping correctly.
fn counter_path(object: &str, instance: Option<&str>, counter: &str) -> Option<String> {
    unsafe {
        let mut object_w = crate::utils::to_wide(object);
        let mut instance_w = instance.map(crate::utils::to_wide);
        let mut counter_w = crate::utils::to_wide(counter);
        let elements = PDH_COUNTER_PATH_ELEMENTS_W {
            szMachineName: std::ptr::null_mut(),
            szObjectName: object_w.as_mut_ptr(),
            szInstanceName: instance_w
                .as_mut()
                .map(|v| v.as_mut_ptr())
                .unwrap_or(std::ptr::null_mut()),
            szParentInstance: std::ptr::null_mut(),
            dwInstanceIndex: 0,
            szCounterName: counter_w.as_mut_ptr(),
        };
        let mut size = 0u32;
        let status = PdhMakeCounterPathW(&elements, std::ptr::null_mut(), &mut size, 0);
        if status != PDH_MORE_DATA || size == 0 {
            return None;
        }
        let mut buf = vec![0u16; size as usize + 1];
        let mut actual = size;
        let status = PdhMakeCounterPathW(&elements, buf.as_mut_ptr(), &mut actual, 0);
        if status != PDH_CSTATUS_VALID_DATA {
            return None;
        }
        Some(crate::utils::wide_to_string(&buf))
    }
}

/// A single-counter PDH query (used by the CPU-frequency probe).
struct PdhCounter {
    query: isize,
    counter: isize,
}

impl PdhCounter {
    /// Open a query with one counter. Returns `None` when the counter object
    /// does not exist on this system.
    fn new(path: &str) -> Option<Self> {
        unsafe {
            let mut query: isize = 0;
            if PdhOpenQueryW(std::ptr::null(), 0, &mut query) != PDH_CSTATUS_VALID_DATA {
                return None;
            }
            let mut counter: isize = 0;
            let status =
                PdhAddEnglishCounterW(query, crate::utils::to_wide(path).as_ptr(), 0, &mut counter);
            if status == PDH_CSTATUS_NO_OBJECT || status == PDH_CSTATUS_VALID_DATA {
                if status == PDH_CSTATUS_NO_OBJECT {
                    PdhCloseQuery(query);
                    return None;
                }
                Some(Self { query, counter })
            } else {
                PdhCloseQuery(query);
                None
            }
        }
    }

    /// Sample twice (first immediately, then after `gap_ms`) and read the
    /// formatted double value.
    fn sample_double(&self, gap_ms: u64) -> Option<f64> {
        unsafe {
            if PdhCollectQueryData(self.query) != PDH_CSTATUS_VALID_DATA {
                return None;
            }
            Sleep(gap_ms as u32);
            if PdhCollectQueryData(self.query) != PDH_CSTATUS_VALID_DATA {
                return None;
            }
            read_double(self.counter)
        }
    }
}

impl Drop for PdhCounter {
    fn drop(&mut self) {
        unsafe { PdhCloseQuery(self.query) };
    }
}

/// Read the formatted double value of one counter already in a query.
unsafe fn read_double(counter: isize) -> Option<f64> {
    let mut value: PDH_FMT_COUNTERVALUE = std::mem::zeroed();
    let mut counter_type: u32 = 0;
    let status = PdhGetFormattedCounterValue(
        counter,
        PDH_FMT_DOUBLE as PDH_FMT,
        &mut counter_type,
        &mut value,
    );
    if status != PDH_CSTATUS_VALID_DATA || value.CStatus != PDH_CSTATUS_VALID_DATA {
        return None;
    }
    let v = value.Anonymous.doubleValue;
    if v.is_finite() {
        Some(v)
    } else {
        None
    }
}

/// Current CPU frequency as a fraction of base from
/// `\Processor Information(_Total)\% Processor Performance`. Returns `None`
/// when the counter is unavailable.
///
/// The counter is a percentage of the base clock and legitimately exceeds
/// 100 when turbo boost raises the multiplier above the base ratio (e.g.
/// 118% on a 2712 MHz base means ~3250 MHz). Only values far outside any
/// plausible multiplier are treated as invalid.
pub fn cpu_performance_percent() -> Option<f64> {
    let path = counter_path(
        PROCESSOR_INFORMATION_OBJECT,
        Some("_Total"),
        "% Processor Performance",
    )?;
    let counter = PdhCounter::new(&path)?;
    let v = counter.sample_double(250)?;
    // Turbo boost pushes this counter past 100; cap at 1000% as a sanity
    // bound against corrupt counter data.
    if (0.0..=1000.0).contains(&v) {
        Some(v)
    } else {
        None
    }
}

/// Enumerate the instances of any PDH object (double-NUL-terminated wide
/// strings), skipping empty names and the aggregate `_Total` instance.
fn enum_object_instances(object: &str) -> Vec<String> {
    unsafe {
        let object_w = crate::utils::to_wide(object);
        let mut counter_size = 0u32;
        let mut instance_size = 0u32;
        let mut status = PdhEnumObjectItemsW(
            std::ptr::null(),
            std::ptr::null(),
            object_w.as_ptr(),
            std::ptr::null_mut(),
            &mut counter_size,
            std::ptr::null_mut(),
            &mut instance_size,
            PERF_DETAIL_WIZARD,
            0,
        );
        if status != PDH_MORE_DATA || instance_size == 0 {
            return Vec::new();
        }
        let mut instance_buf = vec![0u16; instance_size as usize + 2];
        let mut counters_buf = vec![0u16; counter_size as usize + 2];
        status = PdhEnumObjectItemsW(
            std::ptr::null(),
            std::ptr::null(),
            object_w.as_ptr(),
            counters_buf.as_mut_ptr(),
            &mut counter_size,
            instance_buf.as_mut_ptr(),
            &mut instance_size,
            PERF_DETAIL_WIZARD,
            0,
        );
        if status != PDH_CSTATUS_VALID_DATA {
            return Vec::new();
        }
        let mut out = Vec::new();
        let mut i = 0usize;
        while i < instance_buf.len() && instance_buf[i] != 0 {
            let mut end = i;
            while end < instance_buf.len() && instance_buf[end] != 0 {
                end += 1;
            }
            let name = crate::utils::wide_to_string(&instance_buf[i..end]);
            if !name.is_empty() && !name.eq_ignore_ascii_case("_total") {
                out.push(name);
            }
            i = end + 1;
        }
        out
    }
}

/// Physical disk instances (e.g. `0`, `1 C:`, `NVMe`), reusing the generic
/// instance enumerator. The instance list opens with no guaranteed blank
/// placeholder, so only empty names and `_Total` are skipped — never a fixed
/// first position.
fn physical_disk_instances() -> Vec<String> {
    enum_object_instances(PHYSICAL_DISK_OBJECT)
}

/// ACPI thermal-zone temperatures via the PDH counter
/// `\Thermal Zone Information(*)\Temperature`, which is readable without
/// elevation (unlike the `MSAcpi_ThermalZoneTemperature` WMI class on many
/// hosts).
///
/// The counter is a gauge expressed in kelvin; readings that decode to an
/// implausible temperature (below -30 C or above 110 C) are firmware
/// placeholders and are skipped rather than reported as real sensors.
pub fn thermal_zone_temperatures() -> Vec<(String, f64)> {
    let instances = enum_object_instances(THERMAL_ZONE_OBJECT);
    if instances.is_empty() {
        return Vec::new();
    }

    let mut query: isize = 0;
    if unsafe { PdhOpenQueryW(std::ptr::null(), 0, &mut query) } != PDH_CSTATUS_VALID_DATA {
        return Vec::new();
    }
    let mut handles: Vec<(String, isize)> = Vec::new();
    for inst in &instances {
        if let Some(path) = counter_path(THERMAL_ZONE_OBJECT, Some(inst), "Temperature") {
            let mut h: isize = 0;
            let status = unsafe {
                PdhAddEnglishCounterW(query, crate::utils::to_wide(&path).as_ptr(), 0, &mut h)
            };
            if status == PDH_CSTATUS_VALID_DATA {
                handles.push((inst.clone(), h));
            }
        }
    }
    if handles.is_empty() {
        unsafe { PdhCloseQuery(query) };
        return Vec::new();
    }

    // Two samples, one shared gap — the same shape `disk_activity` uses, so a
    // counter that needs a warm-up sample still produces a value.
    let first_ok = unsafe { PdhCollectQueryData(query) } == PDH_CSTATUS_VALID_DATA;
    unsafe { Sleep(100) };
    let second_ok = unsafe { PdhCollectQueryData(query) } == PDH_CSTATUS_VALID_DATA;

    let mut out = Vec::new();
    if first_ok && second_ok {
        for (inst, h) in &handles {
            if let Some(v) = unsafe { read_double(*h) } {
                let celsius = v - 273.15;
                if celsius.is_finite() && (-30.0..=110.0).contains(&celsius) {
                    out.push((inst.clone(), celsius));
                }
            }
        }
    }
    unsafe { PdhCloseQuery(query) };
    out
}

/// Sample per-disk activity over a window. `% Disk Time` and
/// `Current Disk Queue Length` are read as instantaneous averages across the
/// window; transfer counters as rates.
///
/// All counters for all disks live in one PDH query, so the two samples
/// (`PdhCollectQueryData`, sleep `gap_ms`, `PdhCollectQueryData`) run once
/// for the whole report — the total cost is one gap, not one gap per counter
/// per disk. `sample_window_ms` in the result is the requested window, not
/// the measured elapsed time.
pub fn disk_activity(sample_window_ms: u64) -> Result<StorageActivity, WinkitError> {
    let gap_ms = sample_window_ms.min(1_000);

    let instances = physical_disk_instances();
    if instances.is_empty() {
        return Ok(StorageActivity {
            status: "unavailable".into(),
            timestamp: crate::utils::time::format_rfc3339(std::time::SystemTime::now()),
            sample_window_ms,
            disks: Vec::new(),
            completeness: "limited".into(),
            unavailable: vec![UnavailableReading::new(
                "physical_disk",
                "activity",
                SensorAvailability::Unavailable,
                "no PhysicalDisk performance counters were available",
            )],
        });
    }

    let mut query: isize = 0;
    let opened =
        unsafe { PdhOpenQueryW(std::ptr::null(), 0, &mut query) } == PDH_CSTATUS_VALID_DATA;
    if !opened {
        return Ok(StorageActivity {
            status: "unavailable".into(),
            timestamp: crate::utils::time::format_rfc3339(std::time::SystemTime::now()),
            sample_window_ms,
            disks: Vec::new(),
            completeness: "limited".into(),
            unavailable: vec![UnavailableReading::new(
                "physical_disk",
                "activity",
                SensorAvailability::Unavailable,
                "a PDH query for the PhysicalDisk object could not be opened",
            )],
        });
    }

    // `handles[disk][counter]` — `None` when that counter does not exist.
    let mut handles: Vec<Vec<Option<isize>>> =
        vec![vec![None; DISK_COUNTERS.len()]; instances.len()];
    for (di, instance) in instances.iter().enumerate() {
        for (ci, counter) in DISK_COUNTERS.iter().enumerate() {
            if let Some(path) = counter_path(PHYSICAL_DISK_OBJECT, Some(instance), counter) {
                let mut h: isize = 0;
                let status = unsafe {
                    PdhAddEnglishCounterW(query, crate::utils::to_wide(&path).as_ptr(), 0, &mut h)
                };
                if status == PDH_CSTATUS_VALID_DATA {
                    handles[di][ci] = Some(h);
                }
            }
        }
    }

    // Two samples for the whole query: first immediately, second after the
    // gap. Both must succeed for any rate to be readable.
    let first_ok = unsafe { PdhCollectQueryData(query) } == PDH_CSTATUS_VALID_DATA;
    unsafe { Sleep(gap_ms as u32) };
    let second_ok = unsafe { PdhCollectQueryData(query) } == PDH_CSTATUS_VALID_DATA;
    let samples_ok = first_ok && second_ok;

    let mut disks = Vec::with_capacity(instances.len());
    for (di, instance) in instances.iter().enumerate() {
        let mut disk = DiskActivity {
            device: instance.clone(),
            ..Default::default()
        };
        if samples_ok {
            disk.busy_percent = handles[di][0].and_then(|h| unsafe { read_double(h) });
            disk.avg_queue_depth = handles[di][1].and_then(|h| unsafe { read_double(h) });
            disk.read_bytes_per_second = handles[di][2].and_then(|h| unsafe { read_double(h) });
            disk.write_bytes_per_second = handles[di][3].and_then(|h| unsafe { read_double(h) });
            disk.read_per_second = handles[di][4].and_then(|h| unsafe { read_double(h) });
            disk.write_per_second = handles[di][5].and_then(|h| unsafe { read_double(h) });
        }
        disk.availability = if disk.busy_percent.is_some() || disk.avg_queue_depth.is_some() {
            SensorAvailability::Available
        } else {
            SensorAvailability::Unavailable
        };
        if disk.availability != SensorAvailability::Available {
            disk.reason = Some(if samples_ok {
                "no usable counters for this disk".into()
            } else {
                "the PDH samples could not be collected for this disk".into()
            });
        }
        disks.push(disk);
    }
    unsafe { PdhCloseQuery(query) };

    Ok(StorageActivity {
        status: "ok".into(),
        timestamp: crate::utils::time::format_rfc3339(std::time::SystemTime::now()),
        sample_window_ms,
        disks,
        completeness: "full".into(),
        unavailable: Vec::new(),
    })
}

/// CPU current frequency in MHz, from base clock * `% Processor Performance`.
pub fn current_cpu_frequency_mhz(base_clock_mhz: f64) -> Option<f64> {
    let pct = cpu_performance_percent()?;
    let freq = base_clock_mhz * pct / 100.0;
    if freq > 0.0 {
        Some(freq)
    } else {
        log_warn!("% Processor Performance returned 0; frequency unknown");
        None
    }
}
