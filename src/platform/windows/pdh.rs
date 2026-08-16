//! Performance-counter (PDH) helpers for CPU frequency and disk activity.
//!
//! PDH counters need two samples separated by a sleep to produce rates.
//! Every query here is self-contained: open, sample, sleep, sample, read,
//! close. A counter that does not exist (`PDH_CSTATUS_NO_OBJECT`) is
//! reported as `None`/unavailable rather than an error, because Windows
//! hides some counters when the corresponding hardware is absent.

use std::time::Instant;

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
            let mut value: PDH_FMT_COUNTERVALUE = std::mem::zeroed();
            let mut counter_type: u32 = 0;
            let status = PdhGetFormattedCounterValue(
                self.counter,
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
    }
}

impl Drop for PdhCounter {
    fn drop(&mut self) {
        unsafe { PdhCloseQuery(self.query) };
    }
}

/// Current CPU frequency as a fraction of base (0-100) from
/// `\Processor Information(_Total)\% Processor Performance`. Returns `None`
/// when the counter is unavailable.
pub fn cpu_performance_percent() -> Option<f64> {
    let path = counter_path(
        PROCESSOR_INFORMATION_OBJECT,
        Some("_Total"),
        "% Processor Performance",
    )?;
    let counter = PdhCounter::new(&path)?;
    let v = counter.sample_double(250)?;
    if (0.0..=100.0).contains(&v) {
        Some(v)
    } else {
        None
    }
}

/// Enumerate `PhysicalDisk` instances (e.g. `0`, `1 C:`, `NVMe`).
fn physical_disk_instances() -> Vec<String> {
    unsafe {
        let object = crate::utils::to_wide(PHYSICAL_DISK_OBJECT);
        let mut counter_size = 0u32;
        let mut instance_size = 0u32;
        let mut status = PdhEnumObjectItemsW(
            std::ptr::null(),
            std::ptr::null(),
            object.as_ptr(),
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
            object.as_ptr(),
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
        // The instance list is a double-NUL-terminated sequence of wide
        // strings, starting with a "blank" instance name.
        let mut out = Vec::new();
        let mut i = 0usize;
        let mut first = true;
        while i < instance_buf.len() && instance_buf[i] != 0 {
            let mut end = i;
            while end < instance_buf.len() && instance_buf[end] != 0 {
                end += 1;
            }
            if !first {
                out.push(crate::utils::wide_to_string(&instance_buf[i..end]));
            }
            first = false;
            i = end + 1;
        }
        out
    }
}

fn disk_counter(instance: &str, counter: &str, gap_ms: u64) -> Option<f64> {
    let path = counter_path(PHYSICAL_DISK_OBJECT, Some(instance), counter)?;
    let c = PdhCounter::new(&path)?;
    c.sample_double(gap_ms)
}

/// Sample per-disk activity over a window. `% Disk Time` and
/// `Current Disk Queue Length` are read as instantaneous averages across the
/// window; transfer counters as rates.
pub fn disk_activity(sample_window_ms: u64) -> Result<StorageActivity, WinkitError> {
    let started = Instant::now();
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

    let mut disks = Vec::with_capacity(instances.len());
    for instance in &instances {
        let mut disk = DiskActivity {
            device: instance.clone(),
            ..Default::default()
        };
        disk.busy_percent = disk_counter(instance, "% Disk Time", gap_ms);
        disk.avg_queue_depth = disk_counter(instance, "Current Disk Queue Length", gap_ms);
        disk.read_bytes_per_second = disk_counter(instance, "Disk Read Bytes/sec", gap_ms);
        disk.write_bytes_per_second = disk_counter(instance, "Disk Write Bytes/sec", gap_ms);
        disk.read_per_second = disk_counter(instance, "Disk Reads/sec", gap_ms);
        disk.write_per_second = disk_counter(instance, "Disk Writes/sec", gap_ms);
        disk.availability = if disk.busy_percent.is_some() || disk.avg_queue_depth.is_some() {
            SensorAvailability::Available
        } else {
            SensorAvailability::Unavailable
        };
        if disk.availability != SensorAvailability::Available {
            disk.reason = Some("no usable counters for this disk".into());
        }
        disks.push(disk);
    }

    Ok(StorageActivity {
        status: "ok".into(),
        timestamp: crate::utils::time::format_rfc3339(std::time::SystemTime::now()),
        sample_window_ms: started.elapsed().as_millis() as u64,
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
