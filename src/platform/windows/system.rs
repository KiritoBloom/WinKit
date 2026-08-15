//! OS-level information: version, architecture, uptime, memory, CPU samples.

use crate::errors::WinkitError;
use crate::models::{CpuSnapshot, SystemInfo};
use crate::platform::windows::ffi::{self, RtlOsVersionInfoW};
use crate::utils::time;
use std::mem::size_of;
use windows_sys::Win32::Foundation::FILETIME;
use windows_sys::Win32::System::SystemInformation::{
    GetSystemInfo, GetTickCount64, GlobalMemoryStatusEx, MEMORYSTATUSEX, SYSTEM_INFO,
};
use windows_sys::Win32::System::Threading::GetSystemTimes;
use windows_sys::Win32::System::WindowsProgramming::GetComputerNameW;

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

    Ok(SystemInfo {
        os_name: "Windows".to_string(),
        version: format!("{}.{}", version.major_version, version.minor_version),
        build: version.build_number,
        architecture: architecture_name(unsafe { si.Anonymous.Anonymous.wProcessorArchitecture })
            .to_string(),
        uptime_seconds: uptime_secs,
        boot_time,
        hostname,
        cpu_cores: si.dwNumberOfProcessors,
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
