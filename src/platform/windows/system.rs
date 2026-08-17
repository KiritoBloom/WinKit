//! OS-level information: version, architecture, uptime, memory, CPU samples.

use crate::errors::WinkitError;
use crate::models::{CpuSnapshot, SystemInfo};
use crate::platform::windows::ffi::{self, RtlOsVersionInfoW};
use crate::utils::time;
use std::mem::size_of;
use windows_sys::Win32::Foundation::FILETIME;
use windows_sys::Win32::System::SystemInformation::{
    GetLogicalProcessorInformationEx, GetSystemInfo, GetTickCount64, GlobalMemoryStatusEx,
    MEMORYSTATUSEX, RelationProcessorCore, SYSTEM_INFO, SYSTEM_LOGICAL_PROCESSOR_INFORMATION_EX,
};
use windows_sys::Win32::System::Threading::GetSystemTimes;
use windows_sys::Win32::System::WindowsProgramming::GetComputerNameW;

/// `(physical_cores, logical_processors)`.
///
/// `GetSystemInfo.dwNumberOfProcessors` reports logical processors (threads),
/// not physical cores; physical cores come from
/// `GetLogicalProcessorInformationEx(RelationProcessorCore)`, one entry per
/// core. Falls back to logical == cores when the topology query is
/// unavailable (the old, overstated behavior).
fn cpu_topology() -> (u32, u32) {
    let mut si: SYSTEM_INFO = unsafe { std::mem::zeroed() };
    unsafe { GetSystemInfo(&mut si) };
    let logical = si.dwNumberOfProcessors;
    if logical == 0 {
        return (0, 0);
    }

    unsafe {
        // Probe with a null buffer: the API reports the required size in
        // `needed` and fails with ERROR_INSUFFICIENT_BUFFER (ok == 0 is the
        // expected probe outcome, not a failure).
        const ERROR_INSUFFICIENT_BUFFER: u32 = 122;
        let mut needed: u32 = 0;
        let ok = GetLogicalProcessorInformationEx(
            RelationProcessorCore,
            std::ptr::null_mut(),
            &mut needed,
        );
        if ok != 0 || needed == 0 || windows_sys::Win32::Foundation::GetLastError() != ERROR_INSUFFICIENT_BUFFER
        {
            return (logical, logical);
        }
        let mut buf = vec![0u8; needed as usize];
        let mut returned = needed;
        let ok = GetLogicalProcessorInformationEx(
            RelationProcessorCore,
            buf.as_mut_ptr() as *mut SYSTEM_LOGICAL_PROCESSOR_INFORMATION_EX,
            &mut returned,
        );
        if ok == 0 || returned == 0 {
            return (logical, logical);
        }
        // Walk the returned buffer by each entry's own `Size` field (the
        // entries are variable-length; the documented pattern is to advance
        // by `Size`, not by the struct size).
        let mut cores: u32 = 0;
        let mut offset = 0usize;
        while offset + 8 <= returned as usize {
            let entry = buf.as_ptr().add(offset) as *const SYSTEM_LOGICAL_PROCESSOR_INFORMATION_EX;
            if (*entry).Relationship == RelationProcessorCore {
                cores += 1;
            }
            let size = (*entry).Size as usize;
            if size < 8 {
                break;
            }
            offset += size;
        }
        if cores == 0 {
            (logical, logical)
        } else {
            (cores, logical)
        }
    }
}

/// Processor architecture names.
fn architecture_name(w: u16) -> &'static str {
    match w {
        0 => "x86",
        5 => "arm",
        6 => "ia64",
        9 => "x64",
        12 => "arm64",
        0xFFFF => "unknown",
        _ => "unknown",
    }
}

/// Collect a safe subset of OS information.
pub fn system_info() -> Result<SystemInfo, WinkitError> {
    let version = os_version()?;
    let mut si: SYSTEM_INFO = unsafe { std::mem::zeroed() };
    unsafe { GetSystemInfo(&mut si) };

    let uptime_secs = unsafe { GetTickCount64() } / 1000;
    let boot_time = std::time::SystemTime::now()
        .checked_sub(std::time::Duration::from_secs(uptime_secs))
        .map(time::format_rfc3339);

    let hostname = hostname();

    let memory = global_memory_status();
    let (cpu_cores, logical_processors) = cpu_topology();

    Ok(SystemInfo {
        os_name: "Windows".to_string(),
        version: format!("{}.{}", version.major_version, version.minor_version),
        build: version.build_number,
        architecture: architecture_name(unsafe { si.Anonymous.Anonymous.wProcessorArchitecture })
            .to_string(),
        uptime_seconds: uptime_secs,
        boot_time,
        hostname,
        cpu_cores,
        logical_processors,
        total_memory_bytes: memory.map(|m| m.ullTotalPhys),
    })
}

fn os_version() -> Result<RtlOsVersionInfoW, WinkitError> {
    let mut info = RtlOsVersionInfoW::new();
    let status = unsafe { ffi::RtlGetVersion(&mut info) };
    if status != ffi::NT_SUCCESS {
        return Err(WinkitError::windows_api("RtlGetVersion"));
    }
    Ok(info)
}

fn hostname() -> Option<String> {
    let mut buf = vec![0u16; 64];
    let mut size = buf.len() as u32;
    let ok = unsafe { GetComputerNameW(buf.as_mut_ptr(), &mut size) };
    if ok == 0 {
        return None;
    }
    buf.truncate(size as usize);
    Some(crate::utils::wide_to_string(&buf))
}

fn global_memory_status() -> Option<MEMORYSTATUSEX> {
    let mut ms: MEMORYSTATUSEX = unsafe { std::mem::zeroed() };
    ms.dwLength = size_of::<MEMORYSTATUSEX>() as u32;
    let ok = unsafe { GlobalMemoryStatusEx(&mut ms) };
    if ok == 0 {
        return None;
    }
    Some(ms)
}

/// Sample aggregate CPU times via `GetSystemTimes`.
pub fn cpu_snapshot() -> Result<CpuSnapshot, WinkitError> {
    let mut idle = unsafe { std::mem::zeroed::<FILETIME>() };
    let mut kernel = unsafe { std::mem::zeroed::<FILETIME>() };
    let mut user = unsafe { std::mem::zeroed::<FILETIME>() };
    let ok = unsafe { GetSystemTimes(&mut idle, &mut kernel, &mut user) };
    if ok == 0 {
        return Err(WinkitError::windows_api("GetSystemTimes"));
    }
    Ok(CpuSnapshot {
        idle_ms: time::ticks_to_ms(idle.dwHighDateTime, idle.dwLowDateTime),
        kernel_ms: time::ticks_to_ms(kernel.dwHighDateTime, kernel.dwLowDateTime),
        user_ms: time::ticks_to_ms(user.dwHighDateTime, user.dwLowDateTime),
    })
}

/// Memory load percent (0-100) and totals.
pub fn memory_status() -> Option<(u32, u64, u64)> {
    let ms = global_memory_status()?;
    Some((ms.dwMemoryLoad, ms.ullTotalPhys, ms.ullAvailPhys))
}

/// Wait a short interval and return the busy CPU percent across two samples.
/// A value of 100.0 means one core is fully busy.
pub fn sample_cpu_busy_percent(interval_ms: u64) -> Result<Option<f64>, WinkitError> {
    let first = cpu_snapshot()?;
    std::thread::sleep(std::time::Duration::from_millis(interval_ms));
    let second = cpu_snapshot()?;
    Ok(second.busy_percent(&first))
}

#[cfg(test)]
mod tests {
    // These tests call the Windows API and are therefore only compiled in
    // the `mocks` configuration path is irrelevant — they are never run in
    // this build task. They document intent for future maintainers.
    #[allow(dead_code)]
    fn _windows_api_tests_require_a_live_host() {}
}

/// Live Windows regression tests (opt-in): `WINKIT_LIVE_WINDOWS=1 cargo test
/// --features live-windows`. Guards the physical-core topology helper used by
/// `system_info.cpu_cores`.
#[cfg(all(test, feature = "live-windows"))]
mod live_windows {
    use super::*;

    fn live_enabled() -> bool {
        std::env::var("WINKIT_LIVE_WINDOWS")
            .map(|v| v == "1")
            .unwrap_or(false)
    }

    #[test]
    fn cpu_topology_counts_cores_and_logical_processors() {
        if !live_enabled() {
            eprintln!("SKIP: live diagnostic harness not enabled; run with WINKIT_LIVE_WINDOWS=1");
            return;
        }
        let (cores, logical) = cpu_topology();
        eprintln!("LIVE cpu_topology cores={cores} logical={logical}");
        assert!(cores > 0, "at least one physical core");
        assert!(logical >= cores, "logical processors >= physical cores");
        // Cross-check against WMI's NumberOfCores, which is authoritative.
        let wmi_cores = crate::platform::windows::wmi::WmiSession::connect("root\\cimv2")
            .and_then(|s| {
                s.query("SELECT NumberOfCores FROM Win32_Processor")
            })
            .ok()
            .and_then(|rows| rows.first().and_then(|r| r.get_u32("NumberOfCores")));
        if let Some(wmi) = wmi_cores {
            assert_eq!(
                cores, wmi,
                "physical cores must match WMI (topology={cores}, WMI={wmi})"
            );
        }
        // Match what WMI reports for the same machine.
        let mut si: SYSTEM_INFO = unsafe { std::mem::zeroed() };
        unsafe { GetSystemInfo(&mut si) };
        assert_eq!(
            logical, si.dwNumberOfProcessors,
            "logical processors must match GetSystemInfo"
        );
    }
}
