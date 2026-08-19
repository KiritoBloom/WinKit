//! Read-only registry diagnostics from a fixed allowlist of keys.
//!
//! Only the key paths and value names listed in the plan are ever opened;
//! no caller-supplied paths exist, and no binary value content is returned
//! (the StartupApproved flag is parsed into an `enabled` boolean).

use crate::errors::WinkitError;
use crate::models::{
    InstalledSoftware, RegistryCounts, RegistryDiagnostics, StartupProgram, SystemIdentity,
};
use crate::utils::{to_wide, wide_to_string};
use std::ptr::null_mut;
use windows_sys::Win32::Foundation::ERROR_NO_MORE_ITEMS;
use windows_sys::Win32::System::Registry::{
    RegCloseKey, RegEnumKeyExW, RegEnumValueW, RegOpenKeyExW, RegQueryValueExW, HKEY,
    HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE, KEY_READ, REG_DWORD, REG_EXPAND_SZ, REG_SZ,
};

const KEY_WOW64_64KEY: u32 = 0x0100;
const REG_ACCESS: u32 = KEY_READ | KEY_WOW64_64KEY;

const OS_IDENTITY_KEY: &str = "SOFTWARE\\Microsoft\\Windows NT\\CurrentVersion";
const RUN_KEYS: &[(HKEY, &str, &str)] = &[
    (
        HKEY_LOCAL_MACHINE,
        "SOFTWARE\\Microsoft\\Windows\\CurrentVersion\\Run",
        "machine",
    ),
    (
        HKEY_LOCAL_MACHINE,
        "SOFTWARE\\Microsoft\\Windows\\CurrentVersion\\RunOnce",
        "machine",
    ),
    (
        HKEY_LOCAL_MACHINE,
        "SOFTWARE\\WOW6432Node\\Microsoft\\Windows\\CurrentVersion\\Run",
        "machine",
    ),
    (
        HKEY_CURRENT_USER,
        "SOFTWARE\\Microsoft\\Windows\\CurrentVersion\\Run",
        "user",
    ),
    (
        HKEY_CURRENT_USER,
        "SOFTWARE\\Microsoft\\Windows\\CurrentVersion\\RunOnce",
        "user",
    ),
];
const STARTUP_APPROVED_KEY: &str =
    "SOFTWARE\\Microsoft\\Windows\\CurrentVersion\\Explorer\\StartupApproved\\Run";
const UNINSTALL_KEYS: &[&str] = &[
    "SOFTWARE\\Microsoft\\Windows\\CurrentVersion\\Uninstall",
    "SOFTWARE\\WOW6432Node\\Microsoft\\Windows\\CurrentVersion\\Uninstall",
];
const MAX_SOFTWARE: usize = 200;

/// Read the full registry diagnostics view. Failures to open a key are
/// reported in `warnings`; the rest of the view is still returned.
pub fn read_registry_diagnostics(
    include_software: bool,
    max_software: usize,
) -> Result<RegistryDiagnostics, WinkitError> {
    let mut warnings = Vec::new();
    let system_identity = read_system_identity(&mut warnings);
    let startup_programs = read_startup_programs(&mut warnings);
    let installed_software = if include_software {
        read_installed_software(max_software.min(MAX_SOFTWARE), &mut warnings)
    } else {
        Vec::new()
    };
    let counts = RegistryCounts {
        startup_programs: startup_programs.len(),
        installed_software: installed_software.len(),
    };
    Ok(RegistryDiagnostics {
        system_identity,
        startup_programs,
        installed_software,
        counts,
        warnings,
    })
}

fn open_key(root: HKEY, path: &str) -> Result<HKEY, u32> {
    let path_wide = to_wide(path);
    let mut key = null_mut();
    let rc = unsafe { RegOpenKeyExW(root, path_wide.as_ptr(), 0, REG_ACCESS, &mut key) };
    if rc != 0 || key.is_null() {
        return Err(rc);
    }
    Ok(key)
}

fn read_system_identity(warnings: &mut Vec<String>) -> SystemIdentity {
    match open_key(HKEY_LOCAL_MACHINE, OS_IDENTITY_KEY) {
        Ok(key) => {
            let identity = SystemIdentity {
                product_name: read_value_string(key, "ProductName"),
                display_version: read_value_string(key, "DisplayVersion"),
                current_version: read_value_string(key, "CurrentVersion"),
                current_build: read_value_string(key, "CurrentBuildNumber")
                    .or_else(|| read_value_string(key, "CurrentBuild")),
                ubr: read_value_string(key, "UBR"),
                install_date: read_value_dword(key, "InstallDate").and_then(install_date_to_rfc3339),
                edition_id: read_value_string(key, "EditionID"),
                build_lab_ex: read_value_string(key, "BuildLabEx"),
            };
            unsafe { RegCloseKey(key) };
            identity
        }
        Err(_) => {
            warnings.push(format!(
                "unable to open registry key HKLM\\{OS_IDENTITY_KEY}"
            ));
            SystemIdentity::default()
        }
    }
}

fn read_startup_programs(warnings: &mut Vec<String>) -> Vec<StartupProgram> {
    let mut out = Vec::new();
    for (root, path, scope) in RUN_KEYS {
        match open_key(*root, path) {
            Ok(key) => {
                for (name, command) in enum_string_values(key) {
                    let enabled = startup_entry_enabled(*root, &name);
                    out.push(StartupProgram {
                        name,
                        command,
                        scope: (*scope).to_string(),
                        source_key: format!(
                            "{}\\{}",
                            if *root == HKEY_LOCAL_MACHINE { "HKLM" } else { "HKCU" },
                            path
                        ),
                        enabled,
                    });
                }
                unsafe { RegCloseKey(key) };
            }
            Err(_) => warnings.push(format!("unable to open registry key (Run) {path}")),
        }
    }
    out
}

fn startup_entry_enabled(root: HKEY, name: &str) -> bool {
    match open_key(root, STARTUP_APPROVED_KEY) {
        Ok(key) => {
            let bytes = read_value_bytes(key, name);
            unsafe { RegCloseKey(key) };
            startup_approved_enabled(bytes.as_deref().unwrap_or(&[]))
        }
        Err(_) => true,
    }
}

/// State byte at offset 0 of a `StartupApproved\Run` value: `0x02` enabled,
/// `0x03` disabled. Anything else (or an absent entry) means enabled, which
/// matches Task Manager's behavior.
pub fn startup_approved_enabled(data: &[u8]) -> bool {
    match data.first() {
        Some(0x02) => true,
        Some(0x03) => false,
        _ => true,
    }
}

fn read_installed_software(
    max_software: usize,
    warnings: &mut Vec<String>,
) -> Vec<InstalledSoftware> {
    let mut out = Vec::new();
    for path in UNINSTALL_KEYS {
        let Ok(key) = open_key(HKEY_LOCAL_MACHINE, path) else {
            warnings.push(format!("unable to open registry key (Uninstall) {path}"));
            continue;
        };
        for subkey_name in enum_subkeys(key) {
            if out.len() >= max_software {
                break;
            }
            if let Some(entry) = read_uninstall_entry(key, &subkey_name) {
                out.push(entry);
            }
        }
        unsafe { RegCloseKey(key) };
        if out.len() >= max_software {
            break;
        }
    }
    out.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
    out.truncate(max_software);
    out
}

fn read_uninstall_entry(parent: HKEY, subkey_name: &str) -> Option<InstalledSoftware> {
    let subkey_wide = to_wide(subkey_name);
    let mut key = null_mut();
    let rc = unsafe { RegOpenKeyExW(parent, subkey_wide.as_ptr(), 0, REG_ACCESS, &mut key) };
    if rc != 0 || key.is_null() {
        return None;
    }
    let display_name = read_value_string(key, "DisplayName");
    if display_name.is_none() {
        unsafe { RegCloseKey(key) };
        return None;
    }
    let entry = InstalledSoftware {
        name: display_name.unwrap_or_default(),
        version: read_value_string(key, "DisplayVersion"),
        publisher: read_value_string(key, "Publisher"),
        install_date: read_value_string(key, "InstallDate"),
    };
    unsafe { RegCloseKey(key) };
    Some(entry)
}

fn enum_subkeys(key: HKEY) -> Vec<String> {
    let mut out = Vec::new();
    let mut index: u32 = 0;
    loop {
        let mut name_buf = vec![0u16; 256];
        let mut name_len = name_buf.len() as u32;
        let rc = unsafe {
            RegEnumKeyExW(
                key,
                index,
                name_buf.as_mut_ptr(),
                &mut name_len,
                null_mut(),
                null_mut(),
                null_mut(),
                null_mut(),
            )
        };
        if rc == ERROR_NO_MORE_ITEMS {
            break;
        }
        if rc != 0 {
            break;
        }
        out.push(wide_to_string(&name_buf[..name_len as usize]));
        index += 1;
    }
    out
}

fn enum_string_values(key: HKEY) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let mut index: u32 = 0;
    loop {
        let mut name_buf = vec![0u16; 512];
        let mut name_len = name_buf.len() as u32;
        let mut ty: u32 = 0;
        let mut data_len: u32 = 0;
        let rc = unsafe {
            RegEnumValueW(
                key,
                index,
                name_buf.as_mut_ptr(),
                &mut name_len,
                null_mut(),
                &mut ty,
                null_mut(),
                &mut data_len,
            )
        };
        if rc == ERROR_NO_MORE_ITEMS {
            break;
        }
        if rc != 0 {
            break;
        }
        let name = wide_to_string(&name_buf[..name_len as usize]);
        if ty == REG_SZ || ty == REG_EXPAND_SZ {
            let mut data = vec![0u8; data_len.max(1) as usize];
            let mut actual = data_len;
            let rc = unsafe {
                RegQueryValueExW(
                    key,
                    name_buf.as_ptr(),
                    null_mut(),
                    null_mut(),
                    data.as_mut_ptr(),
                    &mut actual,
                )
            };
            if rc == 0 {
                let wide: Vec<u16> = data[..(actual as usize).min(data.len())]
                    .chunks_exact(2)
                    .map(|c| u16::from_le_bytes([c[0], c[1]]))
                    .collect();
                let value = wide_to_string(&wide);
                if !value.is_empty() {
                    out.push((name, value));
                }
            }
        }
        index += 1;
    }
    out
}

fn read_value_string(key: HKEY, name: &str) -> Option<String> {
    let name_wide = to_wide(name);
    let mut len: u32 = 0;
    let rc = unsafe {
        RegQueryValueExW(
            key,
            name_wide.as_ptr(),
            null_mut(),
            null_mut(),
            null_mut(),
            &mut len,
        )
    };
    if rc != 0 || len == 0 {
        return None;
    }
    let mut buf = vec![0u16; (len as usize).div_ceil(2)];
    let mut size = len;
    let rc = unsafe {
        RegQueryValueExW(
            key,
            name_wide.as_ptr(),
            null_mut(),
            null_mut(),
            buf.as_mut_ptr() as *mut u8,
            &mut size,
        )
    };
    if rc != 0 {
        return None;
    }
    let value = wide_to_string(&buf[..(size as usize).min(buf.len() * 2) / 2]);
    if value.is_empty() {
        None
    } else {
        Some(value)
    }
}

fn read_value_dword(key: HKEY, name: &str) -> Option<u32> {
    let name_wide = to_wide(name);
    let mut ty: u32 = 0;
    let mut buf = [0u8; 4];
    let mut size = 4u32;
    let rc = unsafe {
        RegQueryValueExW(
            key,
            name_wide.as_ptr(),
            null_mut(),
            &mut ty,
            buf.as_mut_ptr(),
            &mut size,
        )
    };
    if rc == 0 && ty == REG_DWORD && size == 4 {
        Some(u32::from_le_bytes(buf))
    } else {
        None
    }
}

fn read_value_bytes(key: HKEY, name: &str) -> Option<Vec<u8>> {
    let name_wide = to_wide(name);
    let mut len: u32 = 0;
    let rc = unsafe {
        RegQueryValueExW(
            key,
            name_wide.as_ptr(),
            null_mut(),
            null_mut(),
            null_mut(),
            &mut len,
        )
    };
    if rc != 0 || len == 0 {
        return None;
    }
    let mut buf = vec![0u8; len as usize];
    let mut size = len;
    let rc = unsafe {
        RegQueryValueExW(
            key,
            name_wide.as_ptr(),
            null_mut(),
            null_mut(),
            buf.as_mut_ptr(),
            &mut size,
        )
    };
    if rc != 0 {
        return None;
    }
    buf.truncate(size as usize);
    Some(buf)
}

/// Convert the registry `InstallDate` DWORD (Unix seconds) to RFC3339.
pub fn install_date_to_rfc3339(seconds: u32) -> Option<String> {
    let t = std::time::UNIX_EPOCH + std::time::Duration::from_secs(seconds as u64);
    crate::utils::time::format_rfc3339_opt(t)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn startup_approved_state_byte_rules() {
        // 0x02 at byte 0 = enabled, 0x03 = disabled, anything else/absent = enabled.
        let mut enabled = vec![0u8; 12];
        enabled[0] = 0x02;
        assert!(startup_approved_enabled(&enabled));
        let mut disabled = vec![0u8; 12];
        disabled[0] = 0x03;
        assert!(!startup_approved_enabled(&disabled));
        assert!(startup_approved_enabled(&[0x00, 0x00]));
        assert!(startup_approved_enabled(&[]));
        assert!(startup_approved_enabled(&[0x01]));
    }

    #[test]
    fn install_date_dword_converts_to_rfc3339() {
        assert_eq!(
            install_date_to_rfc3339(0),
            Some("1970-01-01T00:00:00.000Z".to_string())
        );
        assert_eq!(
            install_date_to_rfc3339(1_700_000_000),
            Some("2023-11-14T22:13:20.000Z".to_string())
        );
    }
}