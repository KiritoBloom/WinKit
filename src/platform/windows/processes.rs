//! Process observability via Win32 (read-only).
//!
//! - Snapshot (names, PIDs, parents, thread counts) via Toolhelp.
//! - Memory counters via `K32GetProcessMemoryInfo`.
//! - CPU times and start time via `GetProcessTimes`.
//! - Command line via PEB walk (x64 processes only; gracefully `None`
//!   otherwise).
//! - Executable path via `QueryFullProcessImageNameW`.

use crate::errors::WinkitError;
use crate::models::{CpuTime, ProcessInfo, ProcessMemory, ProcessTreeNode};
use crate::platform::windows::ffi::{self, ProcessBasicInformation, UnicodeString};
use crate::utils::time;
use crate::utils::wide_to_string;
use std::collections::HashMap;
use std::mem::size_of;
use std::ptr::null_mut;
use std::time::Duration;
use windows_sys::Win32::Foundation::{CloseHandle, FILETIME, HANDLE};
use windows_sys::Win32::System::Diagnostics::Debug::ReadProcessMemory;
use windows_sys::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W, TH32CS_SNAPPROCESS,
};
use windows_sys::Win32::System::ProcessStatus::{
    K32EnumProcesses, K32GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS,
};
use windows_sys::Win32::System::Threading::{
    GetProcessTimes, OpenProcess, QueryFullProcessImageNameW, PROCESS_QUERY_INFORMATION,
    PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_VM_READ,
};

/// Sampling window for the two-sample CPU percent in `get_process`.
const CPU_SAMPLE_MS: u64 = 300;

/// A lightweight process entry from the Toolhelp snapshot.
#[derive(Debug, Clone)]
pub struct ProcessEntry {
    pub pid: u32,
    pub ppid: Option<u32>,
    pub name: String,
    pub threads: u32,
    pub priority: u32,
}

/// Take a full process snapshot (names, PIDs, parents).
pub fn snapshot_processes() -> Result<Vec<ProcessEntry>, WinkitError> {
    let snapshot = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) };
    if snapshot.is_null() {
        return Err(WinkitError::windows_api("CreateToolhelp32Snapshot"));
    }
    let mut entries = Vec::new();
    let mut pe: PROCESSENTRY32W = unsafe { std::mem::zeroed() };
    pe.dwSize = size_of::<PROCESSENTRY32W>() as u32;
    let mut ok = unsafe { Process32FirstW(snapshot, &mut pe) };
    while ok != 0 {
        entries.push(ProcessEntry {
            pid: pe.th32ProcessID,
            ppid: (pe.th32ParentProcessID != 0).then_some(pe.th32ParentProcessID),
            name: wide_to_string(&pe.szExeFile),
            threads: pe.cntThreads,
            priority: pe.pcPriClassBase as u32,
        });
        ok = unsafe { Process32NextW(snapshot, &mut pe) };
    }
    unsafe { CloseHandle(snapshot) };
    Ok(entries)
}

/// All PIDs currently running, via `K32EnumProcesses`.
pub fn enum_pids() -> Result<Vec<u32>, WinkitError> {
    let mut needed: u32 = 0;
    let mut count = unsafe { K32EnumProcesses(null_mut(), 0, &mut needed) };
    if count == 0 {
        return Err(WinkitError::windows_api("K32EnumProcesses"));
    }
    let mut pids = vec![0u32; (needed as usize) / size_of::<u32>() + 16];
    let mut cb = (pids.len() * size_of::<u32>()) as u32;
    count = unsafe { K32EnumProcesses(pids.as_mut_ptr(), cb, &mut needed) };
    if count == 0 {
        return Err(WinkitError::windows_api("K32EnumProcesses"));
    }
    cb = needed;
    pids.truncate((cb as usize) / size_of::<u32>());
    pids.retain(|&p| p != 0);
    Ok(pids)
}

/// Open a process handle with the requested access, or `None`.
fn open_process(pid: u32, access: u32) -> Option<HANDLE> {
    let h = unsafe { OpenProcess(access, 0, pid) };
    if h.is_null() {
        None
    } else {
        Some(h)
    }
}

fn memory_counters(handle: HANDLE) -> Option<ProcessMemory> {
    let mut pmc: PROCESS_MEMORY_COUNTERS = unsafe { std::mem::zeroed() };
    pmc.cb = size_of::<PROCESS_MEMORY_COUNTERS>() as u32;
    let ok = unsafe {
        K32GetProcessMemoryInfo(
            handle,
            &mut pmc,
            size_of::<PROCESS_MEMORY_COUNTERS>() as u32,
        )
    };
    if ok == 0 {
        return None;
    }
    Some(ProcessMemory {
        working_set_bytes: pmc.WorkingSetSize as u64,
        private_bytes: pmc.PagefileUsage as u64,
        peak_working_set_bytes: pmc.PeakWorkingSetSize as u64,
    })
}

/// Kernel + user CPU time and start time for a process.
///
/// # Safety
///
/// `handle` must be a valid open handle with `PROCESS_QUERY_LIMITED_INFORMATION`
/// access (or equivalent) for the lifetime of the call.
pub unsafe fn process_times(handle: HANDLE) -> Option<(u64, String)> {
    let mut creation = unsafe { std::mem::zeroed::<FILETIME>() };
    let mut exit = unsafe { std::mem::zeroed::<FILETIME>() };
    let mut kernel = unsafe { std::mem::zeroed::<FILETIME>() };
    let mut user = unsafe { std::mem::zeroed::<FILETIME>() };
    let ok = unsafe { GetProcessTimes(handle, &mut creation, &mut exit, &mut kernel, &mut user) };
    if ok == 0 {
        return None;
    }
    let total_ms = time::ticks_to_ms(kernel.dwHighDateTime, kernel.dwLowDateTime)
        + time::ticks_to_ms(user.dwHighDateTime, user.dwLowDateTime);
    let start = time::filetime_to_rfc3339(creation.dwHighDateTime, creation.dwLowDateTime);
    Some((total_ms, start.unwrap_or_default()))
}

fn executable_path(handle: HANDLE) -> Option<String> {
    let mut buf = vec![0u16; 1024];
    let mut size = buf.len() as u32;
    let ok = unsafe { QueryFullProcessImageNameW(handle, 0, buf.as_mut_ptr(), &mut size) };
    if ok == 0 {
        return None;
    }
    buf.truncate(size as usize);
    Some(wide_to_string(&buf))
}

/// Read a 64-bit pointer from another process's memory.
unsafe fn read_ptr(handle: HANDLE, addr: usize) -> Option<usize> {
    let mut value: usize = 0;
    let mut read: usize = 0;
    let ok = ReadProcessMemory(
        handle,
        addr as *const std::ffi::c_void,
        &mut value as *mut usize as *mut std::ffi::c_void,
        size_of::<usize>(),
        &mut read,
    );
    if ok == 0 || read != size_of::<usize>() {
        None
    } else {
        Some(value)
    }
}

/// Read a `UNICODE_STRING` structure and its buffer from another process.
unsafe fn read_unicode_string(handle: HANDLE, addr: usize) -> Option<String> {
    let mut us: UnicodeString = std::mem::zeroed();
    let mut read: usize = 0;
    let ok = ReadProcessMemory(
        handle,
        addr as *const std::ffi::c_void,
        &mut us as *mut UnicodeString as *mut std::ffi::c_void,
        size_of::<UnicodeString>(),
        &mut read,
    );
    if ok == 0 || read != size_of::<UnicodeString>() || us.buffer.is_null() {
        return None;
    }
    let byte_len = (us.length as usize).min(64 * 1024);
    let mut buf = vec![0u16; byte_len / 2 + 1];
    let mut read: usize = 0;
    let ok = ReadProcessMemory(
        handle,
        us.buffer as *const std::ffi::c_void,
        buf.as_mut_ptr() as *mut std::ffi::c_void,
        byte_len,
        &mut read,
    );
    if ok == 0 {
        return None;
    }
    Some(wide_to_string(&buf))
}

/// Best-effort command-line read for x64 processes (native PEB walk).
///
/// Returns `None` for 32-bit (WOW64) processes and on any failure; this is
/// an honest limitation, never a fabricated value.
unsafe fn read_process_command_line(handle: HANDLE) -> Option<String> {
    #[cfg(target_pointer_width = "64")]
    {
        let mut pbi: ProcessBasicInformation = std::mem::zeroed();
        let status = ffi::NtQueryInformationProcess(
            handle,
            ffi::PROCESS_BASIC_INFORMATION,
            &mut pbi as *mut ProcessBasicInformation as *mut std::ffi::c_void,
            size_of::<ProcessBasicInformation>() as u32,
            null_mut(),
        );
        if status != ffi::NT_SUCCESS || pbi.peb_base_address.is_null() {
            return None;
        }
        // Offsets are stable for x64 Windows 7 through 11:
        // PEB.ProcessParameters = 0x20, ProcessParameters.CommandLine = 0x70.
        let params = read_ptr(handle, pbi.peb_base_address as usize + 0x20)?;
        let cmdline = read_unicode_string(handle, params + 0x70)?;
        let trimmed = cmdline.trim().to_string();
        if trimmed.is_empty() {
            None
        } else {
            Some(trimmed)
        }
    }
    #[cfg(not(target_pointer_width = "64"))]
    {
        let _ = handle;
        None
    }
}

/// Full details for a single PID, including a live two-sample CPU percent
/// (basis `system_capacity_all_cores`) when the process is openable.
pub fn get_process(pid: u32) -> Result<Option<ProcessInfo>, WinkitError> {
    let entries = snapshot_processes()?;
    let entry = entries.iter().find(|e| e.pid == pid);
    if let Some(h) = open_process(pid, PROCESS_QUERY_INFORMATION | PROCESS_VM_READ) {
        let mut detail = build_process_info(entry, pid, &h, true);
        detail.cpu_percent = sample_cpu_percent(pid);
        unsafe { CloseHandle(h) };
        Ok(Some(detail))
    } else {
        Ok(entry.map(|e| entry_only_info(e.clone())))
    }
}

fn build_process_info(
    entry: Option<&ProcessEntry>,
    pid: u32,
    handle: &HANDLE,
    include_command_line: bool,
) -> ProcessInfo {
    let mem = memory_counters(*handle);
    let (cpu_ms, start) = unsafe { process_times(*handle) }.unwrap_or((0, String::new()));
    let path = executable_path(*handle);
    let cmdline = if include_command_line {
        unsafe { read_process_command_line(*handle) }
    } else {
        None
    };
    ProcessInfo {
        pid,
        name: entry.map(|e| e.name.clone()).unwrap_or_else(|| {
            path.as_ref()
                .map(|p| p.split('\\').next_back().unwrap_or("").to_string())
                .unwrap_or_default()
        }),
        parent_pid: entry.and_then(|e| e.ppid),
        executable_path: path,
        command_line: cmdline,
        working_set_bytes: mem.as_ref().map(|m| m.working_set_bytes),
        private_bytes: mem.as_ref().map(|m| m.private_bytes),
        threads: entry.map(|e| e.threads),
        start_time: (!start.is_empty()).then_some(start),
        cpu_time_ms: Some(cpu_ms),
        cpu_percent: None,
    }
}

/// Toolhelp snapshot view for a process whose handle could not be opened.
fn entry_only_info(e: ProcessEntry) -> ProcessInfo {
    ProcessInfo {
        pid: e.pid,
        name: e.name,
        parent_pid: e.ppid,
        executable_path: None,
        command_line: None,
        working_set_bytes: None,
        private_bytes: None,
        threads: Some(e.threads),
        start_time: None,
        cpu_time_ms: None,
        cpu_percent: None,
    }
}

/// Skeleton for a PID with no Toolhelp entry (PID-enumeration fallback).
fn pid_only_info(pid: u32) -> ProcessInfo {
    ProcessInfo {
        pid,
        name: String::new(),
        parent_pid: None,
        executable_path: None,
        command_line: None,
        working_set_bytes: None,
        private_bytes: None,
        threads: None,
        start_time: None,
        cpu_time_ms: None,
        cpu_percent: None,
    }
}

/// Enrich a single Toolhelp entry into a `ProcessInfo`. Access-denied
/// processes keep the snapshot view (null counters). When
/// `include_command_line` is false the expensive PEB walk is skipped.
fn enrich_entry(entry: ProcessEntry, include_command_line: bool) -> ProcessInfo {
    let handle = open_process(entry.pid, PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_VM_READ);
    if let Some(h) = handle {
        let mut info = build_process_info(Some(&entry), entry.pid, &h, include_command_line);
        info.name = if info.name.is_empty() {
            entry.name.clone()
        } else {
            info.name
        };
        unsafe { CloseHandle(h) };
        info
    } else {
        entry_only_info(entry)
    }
}

/// Two-sample CPU percent for one PID over `CPU_SAMPLE_MS`, on the same
/// basis as `application_groups` (`system_capacity_all_cores`). Returns
/// `None` when the process cannot be opened or either sample fails.
fn sample_cpu_percent(pid: u32) -> Option<f64> {
    let first = cpu_time_pair(pid).ok()??;
    let sys_first = crate::platform::windows::system::cpu_snapshot().ok()?;
    std::thread::sleep(Duration::from_millis(CPU_SAMPLE_MS));
    let sys_second = crate::platform::windows::system::cpu_snapshot().ok()?;
    let second = cpu_time_pair(pid).ok()??;
    let proc_delta = second.process_ms.saturating_sub(first.process_ms);
    let sys_delta = sys_second
        .kernel_ms
        .saturating_sub(sys_first.kernel_ms)
        .saturating_add(sys_second.user_ms.saturating_sub(sys_first.user_ms));
    if sys_delta > 0 {
        Some(proc_delta as f64 / sys_delta as f64 * 100.0)
    } else {
        None
    }
}

/// Order a process listing: processes with a readable working set first,
/// sorted by memory descending; the rest follow in PID order. This keeps
/// the interesting (enriched) processes at the top of truncated listings
/// instead of burying them under protected system entries.
fn order_process_listing(mut processes: Vec<ProcessInfo>) -> Vec<ProcessInfo> {
    use std::cmp::Ordering;
    processes.sort_by(|a, b| match (a.working_set_bytes, b.working_set_bytes) {
        (Some(x), Some(y)) => y.cmp(&x),
        (Some(_), None) => Ordering::Less,
        (None, Some(_)) => Ordering::Greater,
        (None, None) => a.pid.cmp(&b.pid),
    });
    processes
}

/// Shared listing body for `list_processes` and `list_processes_minimal`.
///
/// Every process is enriched before truncation, and the result is ordered
/// by working-set memory with readable processes first, so a small
/// `limit` still surfaces the heaviest processes rather than a run of
/// protected system entries with unreadable counters. When
/// `include_command_line` is false the expensive PEB walk is skipped.
fn list_processes_impl(
    limit: usize,
    include_command_line: bool,
) -> Result<Vec<ProcessInfo>, WinkitError> {
    let entries = snapshot_processes()?;
    let mut pids: Vec<u32> = entries.iter().map(|e| e.pid).collect();
    if pids.is_empty() {
        // Toolhelp failed to populate (e.g. restricted context); fall back
        // to the PID enumeration so the tool still returns real data.
        pids = enum_pids()?;
    }
    let by_pid: HashMap<u32, ProcessEntry> = entries.into_iter().map(|e| (e.pid, e)).collect();
    let mut out = Vec::with_capacity(pids.len());
    for pid in pids {
        match by_pid.get(&pid) {
            Some(entry) => out.push(enrich_entry(entry.clone(), include_command_line)),
            // No Toolhelp entry (PID-enumeration fallback): report the PID
            // alone rather than dropping the process silently.
            None => out.push(pid_only_info(pid)),
        }
    }
    Ok(order_process_listing(out).into_iter().take(limit).collect())
}

/// List processes up to `limit`, with memory and CPU-time details. CPU
/// *percentages* are intentionally not computed here (they require two
/// samples per process); use `get_process` for that.
pub fn list_processes(limit: usize) -> Result<Vec<ProcessInfo>, WinkitError> {
    list_processes_impl(limit, true)
}

/// Like `list_processes`, but skips the expensive PEB command-line walk:
/// every process reports `command_line: None`. Intended for health
/// aggregation, which does not need command lines.
pub fn list_processes_minimal(limit: usize) -> Result<Vec<ProcessInfo>, WinkitError> {
    list_processes_impl(limit, false)
}

/// Build a process tree rooted at `pid`, bounded by depth and node count.
pub fn process_tree(
    pid: u32,
    max_depth: u32,
    max_nodes: usize,
) -> Result<Option<ProcessTreeNode>, WinkitError> {
    let entries = snapshot_processes()?;
    if !entries.iter().any(|e| e.pid == pid) {
        return Ok(None);
    }
    let mut by_parent: HashMap<u32, Vec<ProcessEntry>> = HashMap::new();
    for e in &entries {
        if let Some(ppid) = e.ppid {
            by_parent.entry(ppid).or_default().push(e.clone());
        }
    }
    // PID -> entry index so each node resolves itself in O(1) instead of a
    // linear scan of every entry (the old implementation was O(n²) overall).
    let index: HashMap<u32, &ProcessEntry> = entries.iter().map(|e| (e.pid, e)).collect();
    let mut budget = max_nodes;
    let root = build_node(pid, 0, max_depth, &by_parent, &index, &mut budget);
    Ok(Some(root))
}

fn build_node(
    pid: u32,
    depth: u32,
    max_depth: u32,
    by_parent: &HashMap<u32, Vec<ProcessEntry>>,
    index: &HashMap<u32, &ProcessEntry>,
    budget: &mut usize,
) -> ProcessTreeNode {
    if *budget == 0 {
        return ProcessTreeNode {
            pid,
            name: String::new(),
            parent_pid: None,
            working_set_bytes: None,
            threads: None,
            cpu_time_ms: None,
            depth,
            children: Vec::new(),
        };
    }
    *budget -= 1;
    let entry = index.get(&pid).copied().cloned();
    let mut children = Vec::new();
    if depth < max_depth {
        if let Some(kids) = by_parent.get(&pid) {
            for kid in kids {
                if *budget == 0 {
                    break;
                }
                children.push(build_node(kid.pid, depth + 1, max_depth, by_parent, index, budget));
            }
        }
    }
    let handle = open_process(pid, PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_VM_READ);
    let (mem, cpu, threads) = handle
        .map(|h| {
            let m = memory_counters(h);
            let c = unsafe { process_times(h) }.map(|t| t.0);
            let t = entry.as_ref().map(|e| e.threads);
            unsafe { CloseHandle(h) };
            (m, c, t)
        })
        .unwrap_or((None, None, None));
    ProcessTreeNode {
        pid,
        name: entry.as_ref().map(|e| e.name.clone()).unwrap_or_default(),
        parent_pid: entry.as_ref().and_then(|e| e.ppid),
        working_set_bytes: mem.map(|m| m.working_set_bytes),
        threads,
        cpu_time_ms: cpu,
        depth,
        children,
    }
}

/// Keep only snapshot entries whose name contains `needle_lower`
/// (case-insensitive). Pure, so the name-matching semantics of
/// `find_process` are testable without a live host.
fn filter_by_name(entries: Vec<ProcessEntry>, needle_lower: &str) -> Vec<ProcessEntry> {
    entries
        .into_iter()
        .filter(|e| e.name.to_lowercase().contains(needle_lower))
        .collect()
}

/// Find processes whose name contains `needle` (case-insensitive).
///
/// The full snapshot is scanned first — not just the top-N by memory — so a
/// low-memory match is still found. Matches are enriched, ordered by working
/// set (readable first), and truncated to `limit`.
pub fn find_process(needle: &str, limit: usize) -> Result<Vec<ProcessInfo>, WinkitError> {
    let entries = snapshot_processes()?;
    let matched = filter_by_name(entries, &needle.to_lowercase());
    let enriched: Vec<ProcessInfo> = matched.into_iter().map(|e| enrich_entry(e, true)).collect();
    Ok(order_process_listing(enriched).into_iter().take(limit).collect())
}

/// Resolve a PID to a process name via a snapshot (used by network/window
/// joins). Returns `None` if the process is gone.
pub fn pid_to_name(pid: u32) -> Option<String> {
    let handle = open_process(pid, PROCESS_QUERY_LIMITED_INFORMATION)?;
    let name = executable_path(handle)
        .map(|p| p.split('\\').next_back().unwrap_or("").to_string())
        .filter(|s| !s.is_empty());
    unsafe { CloseHandle(handle) };
    name.or_else(|| {
        snapshot_processes().ok().and_then(|entries| {
            entries
                .iter()
                .find(|e| e.pid == pid)
                .map(|e| e.name.clone())
        })
    })
}

/// Re-export CPU time pair used by the diagnostics engine.
pub fn cpu_time_pair(pid: u32) -> Result<Option<CpuTime>, WinkitError> {
    let handle = match open_process(pid, PROCESS_QUERY_LIMITED_INFORMATION) {
        Some(h) => h,
        None => return Ok(None),
    };
    let proc_ms = unsafe { process_times(handle) }.map(|t| t.0);
    let sys = crate::platform::windows::system::cpu_snapshot().ok();
    unsafe { CloseHandle(handle) };
    match (proc_ms, sys) {
        (Some(process_ms), Some(s)) => Ok(Some(CpuTime {
            system_ms: s.kernel_ms + s.user_ms,
            process_ms,
        })),
        _ => Ok(None),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn proc(pid: u32, working_set: Option<u64>) -> ProcessInfo {
        ProcessInfo {
            pid,
            name: format!("p{pid}"),
            parent_pid: None,
            executable_path: None,
            command_line: None,
            working_set_bytes: working_set,
            private_bytes: None,
            threads: None,
            start_time: None,
            cpu_time_ms: None,
            cpu_percent: None,
        }
    }

    #[test]
    fn listing_orders_readable_memory_first() {
        let ordered = order_process_listing(vec![
            proc(1, None),
            proc(2, Some(50)),
            proc(3, Some(200)),
            proc(4, None),
        ]);
        let pids: Vec<u32> = ordered.iter().map(|p| p.pid).collect();
        assert_eq!(pids, vec![3, 2, 1, 4]);
    }

    #[test]
    fn listing_falls_back_to_pid_order_for_unreadable_processes() {
        let ordered = order_process_listing(vec![proc(9, None), proc(2, None), proc(7, None)]);
        let pids: Vec<u32> = ordered.iter().map(|p| p.pid).collect();
        assert_eq!(pids, vec![2, 7, 9]);
    }

    #[test]
    fn find_process_name_filter_is_case_insensitive_substring() {
        let entries = vec![
            ProcessEntry {
                pid: 1,
                ppid: None,
                name: "chrome.exe".into(),
                threads: 0,
                priority: 0,
            },
            ProcessEntry {
                pid: 2,
                ppid: None,
                name: "Firefox".into(),
                threads: 0,
                priority: 0,
            },
            ProcessEntry {
                pid: 3,
                ppid: None,
                name: "CHROME_UPDATE.EXE".into(),
                threads: 0,
                priority: 0,
            },
            ProcessEntry {
                pid: 4,
                ppid: None,
                name: "node".into(),
                threads: 0,
                priority: 0,
            },
        ];
        let matched = filter_by_name(entries, "chrome");
        let pids: Vec<u32> = matched.iter().map(|e| e.pid).collect();
        assert_eq!(pids, vec![1, 3]);
    }
}
