//! Service observability via Win32 SCM (read-only).

use crate::errors::WinkitError;
use crate::models::ServiceInfo;
use crate::utils::wide_to_string;
use std::mem::size_of;
use std::ptr::null_mut;
use windows_sys::Win32::Foundation::{
    CloseHandle, ERROR_INSUFFICIENT_BUFFER, ERROR_MORE_DATA, HANDLE,
};
use windows_sys::Win32::System::Registry::{
    RegCloseKey, RegOpenKeyExW, RegQueryValueExW, HKEY_LOCAL_MACHINE, KEY_READ,
};
use windows_sys::Win32::System::Services::{
    EnumServicesStatusExW, OpenSCManagerW, OpenServiceW, QueryServiceConfigW, QueryServiceStatusEx,
    ENUM_SERVICE_STATUS_PROCESSW, QUERY_SERVICE_CONFIGW, SC_ENUM_PROCESS_INFO,
    SC_MANAGER_ENUMERATE_SERVICE, SC_STATUS_PROCESS_INFO, SERVICE_QUERY_CONFIG,
    SERVICE_QUERY_STATUS, SERVICE_STATE_ALL, SERVICE_WIN32,
};

fn state_name(state: u32) -> &'static str {
    match state {
        1 => "stopped",
        2 => "start_pending",
        3 => "stop_pending",
        4 => "running",
        5 => "continue_pending",
        6 => "pause_pending",
        7 => "paused",
        _ => "unknown",
    }
}

fn service_type_name(t: u32) -> String {
    let mut parts = Vec::new();
    if t & 0x00000010 != 0 {
        parts.push("win32_own_process");
    }
    if t & 0x00000020 != 0 {
        parts.push("win32_share_process");
    }
    if t & 0x00000001 != 0 {
        parts.push("kernel");
    }
    if t & 0x00000002 != 0 {
        parts.push("file_system");
    }
    if t & 0x00000008 != 0 {
        parts.push("recognizer");
    }
    if t & 0x00000100 != 0 {
        parts.push("interactive");
    }
    if parts.is_empty() {
        parts.push("unknown");
    }
    parts.join(",")
}

fn start_type_name(t: u32) -> &'static str {
    match t {
        0 => "boot",
        1 => "system",
        2 => "auto",
        3 => "manual",
        4 => "disabled",
        _ => "unknown",
    }
}

fn open_scm() -> Result<HANDLE, WinkitError> {
    let h = unsafe { OpenSCManagerW(null_mut(), null_mut(), SC_MANAGER_ENUMERATE_SERVICE) };
    if h.is_null() {
        return Err(WinkitError::windows_api("OpenSCManagerW"));
    }
    Ok(h)
}

/// Read a NUL-terminated wide string pointer that lives inside `buffer`,
/// bounded by the buffer so the read can never go out of bounds.
fn pwstr_in_buffer(ptr: *const u16, buffer_start: usize, buffer_len: usize) -> String {
    if ptr.is_null() {
        return String::new();
    }
    let off = ptr as usize - buffer_start;
    if off >= buffer_len {
        return String::new();
    }
    let max_chars = (buffer_len - off) / 2;
    wide_to_string(unsafe { std::slice::from_raw_parts(ptr, max_chars) })
}

/// The SCM does not expose a service's display name through the config APIs
/// in windows-sys 0.59, so read it from the registry where SCM stores it.
fn registry_display_name(service_name: &str) -> String {
    let subkey = format!("SYSTEM\\CurrentControlSet\\Services\\{service_name}");
    let subkey_wide = crate::utils::to_wide(&subkey);
    let mut key = null_mut();
    let rc = unsafe {
        RegOpenKeyExW(
            HKEY_LOCAL_MACHINE,
            subkey_wide.as_ptr(),
            0,
            KEY_READ,
            &mut key,
        )
    };
    if rc != 0 || key.is_null() {
        return String::new();
    }
    let value_wide = crate::utils::to_wide("DisplayName");
    let mut len: u32 = 0;
    let rc = unsafe {
        RegQueryValueExW(
            key,
            value_wide.as_ptr(),
            null_mut(),
            null_mut(),
            null_mut(),
            &mut len,
        )
    };
    let out = if rc == 0 && len > 0 {
        let mut buf = vec![0u16; (len as usize + 1) / 2];
        let mut size = len;
        let rc = unsafe {
            RegQueryValueExW(
                key,
                value_wide.as_ptr(),
                null_mut(),
                null_mut(),
                buf.as_mut_ptr() as *mut u8,
                &mut size,
            )
        };
        if rc == 0 {
            wide_to_string(&buf[..(size as usize).min(buf.len() * 2) / 2])
        } else {
            String::new()
        }
    } else {
        String::new()
    };
    unsafe { RegCloseKey(key) };
    out
}

/// List services, bounded by `limit`.
pub fn list_services(limit: usize) -> Result<Vec<ServiceInfo>, WinkitError> {
    let scm = open_scm()?;
    let mut needed: u32 = 0;
    let mut returned: u32 = 0;
    let mut resume: u32 = 0;
    let mut ok = unsafe {
        EnumServicesStatusExW(
            scm,
            SC_ENUM_PROCESS_INFO,
            SERVICE_WIN32,
            SERVICE_STATE_ALL,
            null_mut(),
            0,
            &mut needed,
            &mut returned,
            &mut resume,
            null_mut(),
        )
    };
    if ok == 0 {
        let err = unsafe { windows_sys::Win32::Foundation::GetLastError() };
        if err != ERROR_MORE_DATA && err != ERROR_INSUFFICIENT_BUFFER {
            unsafe { CloseHandle(scm) };
            return Err(WinkitError::windows_api("EnumServicesStatusExW"));
        }
    }
    let mut buf = vec![0u8; needed as usize];
    ok = unsafe {
        EnumServicesStatusExW(
            scm,
            SC_ENUM_PROCESS_INFO,
            SERVICE_WIN32,
            SERVICE_STATE_ALL,
            buf.as_mut_ptr(),
            needed,
            &mut needed,
            &mut returned,
            &mut resume,
            null_mut(),
        )
    };
    if ok == 0 {
        unsafe { CloseHandle(scm) };
        return Err(WinkitError::windows_api("EnumServicesStatusExW"));
    }
    let count = (returned as usize).min(limit);
    let buf_start = buf.as_ptr() as usize;
    let buf_len = buf.len();
    let entries = unsafe {
        std::slice::from_raw_parts(buf.as_ptr() as *const ENUM_SERVICE_STATUS_PROCESSW, count)
    };
    let mut out = Vec::with_capacity(count);
    for e in entries {
        let status = &e.ServiceStatusProcess;
        out.push(ServiceInfo {
            name: pwstr_in_buffer(e.lpServiceName, buf_start, buf_len),
            display_name: pwstr_in_buffer(e.lpDisplayName, buf_start, buf_len),
            state: state_name(status.dwCurrentState).to_string(),
            service_type: service_type_name(status.dwServiceType),
            process_id: (status.dwProcessId != 0).then_some(status.dwProcessId),
            win32_exit_code: (status.dwWin32ExitCode != 0).then_some(status.dwWin32ExitCode),
            start_type: None,
            binary_path: None,
            service_start_name: None,
        });
    }
    unsafe { CloseHandle(scm) };
    Ok(out)
}

/// Detailed view of one service.
pub fn get_service(name: &str) -> Result<Option<ServiceInfo>, WinkitError> {
    let scm = open_scm()?;
    let name_wide = crate::utils::to_wide(name);
    let handle = unsafe {
        OpenServiceW(
            scm,
            name_wide.as_ptr(),
            SERVICE_QUERY_CONFIG | SERVICE_QUERY_STATUS,
        )
    };
    if handle.is_null() {
        unsafe { CloseHandle(scm) };
        return Ok(None);
    }

    let mut info = ServiceInfo {
        name: name.to_string(),
        display_name: registry_display_name(name),
        state: "unknown".to_string(),
        service_type: "unknown".to_string(),
        process_id: None,
        win32_exit_code: None,
        start_type: None,
        binary_path: None,
        service_start_name: None,
    };

    // Status + PID.
    let mut status: windows_sys::Win32::System::Services::SERVICE_STATUS_PROCESS =
        unsafe { std::mem::zeroed() };
    let mut needed: u32 = 0;
    let ok = unsafe {
        QueryServiceStatusEx(
            handle,
            SC_STATUS_PROCESS_INFO,
            &mut status as *mut _ as *mut u8,
            size_of::<windows_sys::Win32::System::Services::SERVICE_STATUS_PROCESS>() as u32,
            &mut needed,
        )
    };
    if ok != 0 {
        info.state = state_name(status.dwCurrentState).to_string();
        info.service_type = service_type_name(status.dwServiceType);
        info.process_id = (status.dwProcessId != 0).then_some(status.dwProcessId);
        info.win32_exit_code = (status.dwWin32ExitCode != 0).then_some(status.dwWin32ExitCode);
    }

    // Config: binary path, start type, account.
    let mut needed: u32 = 0;
    let ok = unsafe { QueryServiceConfigW(handle, null_mut(), 0, &mut needed) };
    if ok == 0 {
        let err = unsafe { windows_sys::Win32::Foundation::GetLastError() };
        if err == ERROR_INSUFFICIENT_BUFFER {
            let mut buf = vec![0u8; needed as usize];
            let ok = unsafe {
                QueryServiceConfigW(
                    handle,
                    buf.as_mut_ptr() as *mut QUERY_SERVICE_CONFIGW,
                    needed,
                    &mut needed,
                )
            };
            if ok != 0 {
                let buf_start = buf.as_ptr() as usize;
                let buf_len = buf.len();
                let cfg = unsafe { &*(buf.as_ptr() as *const QUERY_SERVICE_CONFIGW) };
                info.start_type = Some(start_type_name(cfg.dwStartType).to_string());
                info.binary_path = Some(pwstr_in_buffer(cfg.lpBinaryPathName, buf_start, buf_len));
                info.service_start_name =
                    Some(pwstr_in_buffer(cfg.lpServiceStartName, buf_start, buf_len));
            }
        }
    }

    unsafe {
        CloseHandle(handle);
        CloseHandle(scm);
    }
    Ok(Some(info))
}

/// Running/total service counts for `snapshot`.
pub fn service_summary() -> (usize, usize) {
    match list_services(10_000) {
        Ok(list) => {
            let running = list.iter().filter(|s| s.state == "running").count();
            (running, list.len())
        }
        Err(_) => (0, 0),
    }
}
