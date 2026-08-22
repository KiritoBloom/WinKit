//! Read-only registry diagnostics from a fixed allowlist of keys.
//!
//! Only the key paths and value names listed in the plan are ever opened;
//! no caller-supplied paths exist, and no binary value content is returned
//! (the StartupApproved flag is parsed into an `enabled` boolean).

use crate::errors::WinkitError;
use crate::models::{
    assess_startup_impact, extract_executable_path, InstalledSoftware, RegistryCounts,
    RegistryDiagnostics, StartupProgram, SystemIdentity,
};
use crate::utils::{to_wide, wide_to_string};
use std::path::PathBuf;
use std::ptr::null_mut;
use windows_sys::Win32::Foundation::ERROR_NO_MORE_ITEMS;
use windows_sys::Win32::System::Registry::{
    RegCloseKey, RegEnumKeyExW, RegEnumValueW, RegOpenKeyExW, RegQueryValueExW, HKEY,
    HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE, KEY_READ, REG_DWORD, REG_EXPAND_SZ, REG_MULTI_SZ,
    REG_SZ,
};

const KEY_WOW64_64KEY: u32 = 0x0100;
const REG_ACCESS: u32 = KEY_READ | KEY_WOW64_64KEY;

const OS_IDENTITY_KEY: &str = "SOFTWARE\\Microsoft\\Windows NT\\CurrentVersion";

/// One allowlisted Run/RunOnce source key with the `StartupApproved`
/// subkey(s) that carry its enabled/disabled state. `hidden` marks entries
/// Windows does not surface in Task Manager's Startup apps list.
struct RunSource {
    root: HKEY,
    key_path: &'static str,
    scope: &'static str,
    entry_type: &'static str,
    hidden: bool,
    /// Tried in order; the first key that exists and contains the value
    /// name decides the state (Task Manager writes per-scope variants).
    approved: &'static [&'static str],
}

const APPROVED_RUN: &str =
    "SOFTWARE\\Microsoft\\Windows\\CurrentVersion\\Explorer\\StartupApproved\\Run";
const APPROVED_RUN32: &str =
    "SOFTWARE\\Microsoft\\Windows\\CurrentVersion\\Explorer\\StartupApproved\\Run32";
const APPROVED_RUN_ONCE: &str =
    "SOFTWARE\\Microsoft\\Windows\\CurrentVersion\\Explorer\\StartupApproved\\RunOnce";
const RUN_SOURCES: &[RunSource] = &[
    RunSource {
        root: HKEY_LOCAL_MACHINE,
        key_path: "SOFTWARE\\Microsoft\\Windows\\CurrentVersion\\Run",
        scope: "machine",
        entry_type: "run",
        hidden: false,
        approved: &[APPROVED_RUN],
    },
    RunSource {
        root: HKEY_LOCAL_MACHINE,
        key_path: "SOFTWARE\\Microsoft\\Windows\\CurrentVersion\\RunOnce",
        scope: "machine",
        entry_type: "run_once",
        // Task Manager's Startup apps list does not show RunOnce entries.
        hidden: true,
        approved: &[APPROVED_RUN_ONCE],
    },
    RunSource {
        root: HKEY_LOCAL_MACHINE,
        key_path: "SOFTWARE\\WOW6432Node\\Microsoft\\Windows\\CurrentVersion\\Run",
        scope: "machine",
        entry_type: "run",
        hidden: false,
        approved: &[
            "SOFTWARE\\WOW6432Node\\Microsoft\\Windows\\CurrentVersion\\Explorer\\StartupApproved\\Run",
            APPROVED_RUN32,
        ],
    },
    RunSource {
        root: HKEY_CURRENT_USER,
        key_path: "SOFTWARE\\Microsoft\\Windows\\CurrentVersion\\Run",
        scope: "user",
        entry_type: "run",
        hidden: false,
        approved: &[APPROVED_RUN],
    },
    RunSource {
        root: HKEY_CURRENT_USER,
        key_path: "SOFTWARE\\Microsoft\\Windows\\CurrentVersion\\RunOnce",
        scope: "user",
        entry_type: "run_once",
        hidden: true,
        approved: &[APPROVED_RUN_ONCE],
    },
];

const WINLOGON_KEY: &str = "SOFTWARE\\Microsoft\\Windows NT\\CurrentVersion\\Winlogon";
/// Winlogon values that launch executables, checked in this order.
const WINLOGON_VALUES: &[&str] = &["Userinit", "Shell", "AppSetup", "Taskman", "UIHost"];
const ACTIVE_SETUP_KEYS: &[&str] = &[
    "SOFTWARE\\Microsoft\\Active Setup\\Installed Components",
    "SOFTWARE\\WOW6432Node\\Microsoft\\Active Setup\\Installed Components",
];
const APPROVED_STARTUP_FOLDER: &str =
    "SOFTWARE\\Microsoft\\Windows\\CurrentVersion\\Explorer\\StartupApproved\\StartupFolder";
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
                ubr: read_value_dword(key, "UBR").map(|v| v.to_string()),
                install_date: read_value_dword(key, "InstallDate")
                    .and_then(install_date_to_rfc3339),
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

/// Intermediate entry before impact enrichment.
struct RawStartup {
    name: String,
    command: String,
    scope: String,
    source_key: String,
    enabled: bool,
    entry_type: String,
    hidden: bool,
}

fn read_startup_programs(warnings: &mut Vec<String>) -> Vec<StartupProgram> {
    let mut raw = Vec::new();

    // 1. Run/RunOnce registry keys (HKLM, HKCU, WOW6432Node).
    for src in RUN_SOURCES {
        match open_key(src.root, src.key_path) {
            Ok(key) => {
                for (name, command) in enum_string_values(key) {
                    let enabled = startup_entry_enabled(src.root, src.approved, &name);
                    raw.push(RawStartup {
                        name,
                        command,
                        scope: src.scope.to_string(),
                        source_key: format!(
                            "{}\\{}",
                            if src.root == HKEY_LOCAL_MACHINE {
                                "HKLM"
                            } else {
                                "HKCU"
                            },
                            src.key_path
                        ),
                        enabled,
                        entry_type: src.entry_type.to_string(),
                        hidden: src.hidden,
                    });
                }
                unsafe { RegCloseKey(key) };
            }
            Err(_) => warnings.push(format!(
                "unable to open registry key (Run) {}",
                src.key_path
            )),
        }
    }

    // 2. Winlogon boot-phase executables (hidden from Startup apps).
    read_winlogon_entries(&mut raw, warnings);

    // 3. Session Manager BootExecute (runs before logon; hidden).
    read_boot_execute_entry(&mut raw, warnings);

    // 4. Active Setup StubPath components (per-user logon stubs; hidden).
    read_active_setup_entries(&mut raw, warnings);

    // 5. Startup folder items (user + all-users .lnk files).
    read_startup_folder_entries(&mut raw, warnings);

    let out: Vec<StartupProgram> = raw
        .into_iter()
        .map(|entry| {
            // Expand %VAR% segments for the size probe only; the reported
            // command keeps its original text.
            let exe_size = extract_executable_path(&entry.command)
                .map(crate::platform::windows::environment::expand_env_vars)
                .and_then(|path| std::fs::metadata(path).ok())
                .map(|meta| meta.len());
            let (impact, reasons) =
                assess_startup_impact(&entry.entry_type, &entry.command, exe_size, entry.enabled);
            StartupProgram {
                name: entry.name,
                command: entry.command,
                scope: entry.scope,
                source_key: entry.source_key,
                enabled: entry.enabled,
                entry_type: entry.entry_type,
                hidden: entry.hidden,
                impact,
                impact_reasons: reasons,
            }
        })
        .collect();
    out
}

/// Split a Winlogon value (comma-separated executables) into individual
/// commands, dropping empty segments (e.g. the trailing `"userinit.exe,"`).
fn split_winlogon_command(value: &str) -> Vec<String> {
    value
        .split(',')
        .map(|segment| segment.trim().to_string())
        .filter(|segment| !segment.is_empty())
        .collect()
}

/// Winlogon values are comma-separated lists of executables run during
/// logon. Each non-empty segment becomes one entry; there is no approval
/// mechanism, so these always report as enabled.
fn read_winlogon_entries(out: &mut Vec<RawStartup>, warnings: &mut Vec<String>) {
    const ROOT_TEXT: &str = "HKLM";
    let Ok(key) = open_key(HKEY_LOCAL_MACHINE, WINLOGON_KEY) else {
        warnings.push(format!(
            "unable to open registry key (Winlogon) {WINLOGON_KEY}"
        ));
        return;
    };
    for value_name in WINLOGON_VALUES {
        if let Some(value) = read_value_string(key, value_name) {
            let segments = split_winlogon_command(&value);
            let multiple = segments.len() > 1;
            for (index, segment) in segments.into_iter().enumerate() {
                let name = if multiple {
                    format!("{value_name} #{}", index + 1)
                } else {
                    (*value_name).to_string()
                };
                out.push(RawStartup {
                    name,
                    command: segment,
                    scope: "machine".to_string(),
                    source_key: format!("{ROOT_TEXT}\\{WINLOGON_KEY}"),
                    enabled: true,
                    entry_type: "winlogon".to_string(),
                    hidden: true,
                });
            }
        }
    }
    unsafe { RegCloseKey(key) };
}

/// `Session Manager\BootExecute` is a REG_MULTI_SZ executed by smss at
/// boot, before any user session exists.
fn read_boot_execute_entry(out: &mut Vec<RawStartup>, warnings: &mut Vec<String>) {
    let Ok(key) = open_key(HKEY_LOCAL_MACHINE, SESSION_MANAGER) else {
        warnings.push(format!(
            "unable to open registry key (Session Manager) {SESSION_MANAGER}"
        ));
        return;
    };
    for (index, command) in read_value_multi_sz(key, "BootExecute")
        .into_iter()
        .filter(|s| !s.trim().is_empty())
        .enumerate()
    {
        let name = if index == 0 {
            "BootExecute".to_string()
        } else {
            format!("BootExecute #{}", index + 1)
        };
        out.push(RawStartup {
            name,
            command,
            scope: "machine".to_string(),
            source_key: format!("HKLM\\{SESSION_MANAGER}"),
            enabled: true,
            entry_type: "boot_execute".to_string(),
            hidden: true,
        });
    }
    unsafe { RegCloseKey(key) };
}

/// Open an absolute HKLM subkey path (root key + full path).
fn open_hklm_path(path: &str) -> Option<HKEY> {
    let path_wide = to_wide(path);
    let mut key = null_mut();
    let rc = unsafe {
        RegOpenKeyExW(
            HKEY_LOCAL_MACHINE,
            path_wide.as_ptr(),
            0,
            REG_ACCESS,
            &mut key,
        )
    };
    if rc == 0 && !key.is_null() {
        Some(key)
    } else {
        None
    }
}

/// Active Setup runs each component's `StubPath` once per user at logon.
/// `IsInstalled = 0` marks a component disabled.
fn read_active_setup_entries(out: &mut Vec<RawStartup>, warnings: &mut Vec<String>) {
    for base_path in ACTIVE_SETUP_KEYS {
        let Ok(base) = open_key(HKEY_LOCAL_MACHINE, base_path) else {
            warnings.push(format!(
                "unable to open registry key (Active Setup) {base_path}"
            ));
            continue;
        };
        for subkey_name in enum_subkeys(base) {
            let subkey_path = format!("{base_path}\\{subkey_name}");
            let Some(subkey) = open_hklm_path(&subkey_path) else {
                continue;
            };
            let stub = read_value_string(subkey, "StubPath");
            let display = read_value_string(subkey, "")
                .filter(|s| !s.trim().is_empty())
                .unwrap_or_else(|| subkey_name.clone());
            let installed = read_value_dword(subkey, "IsInstalled");
            unsafe { RegCloseKey(subkey) };
            let Some(stub) = stub.filter(|s| !s.trim().is_empty()) else {
                continue;
            };
            out.push(RawStartup {
                name: display,
                command: stub,
                scope: "machine".to_string(),
                source_key: format!("HKLM\\{subkey_path}"),
                enabled: installed != Some(0),
                entry_type: "active_setup".to_string(),
                hidden: true,
            });
        }
        unsafe { RegCloseKey(base) };
    }
}

/// Explorer metadata files inside a Startup folder (`desktop.ini`,
/// dotfiles) are never autostart entries.
fn is_startup_folder_metadata(file_name: &str) -> bool {
    let lower = file_name.to_ascii_lowercase();
    lower == "desktop.ini" || lower.starts_with('.')
}

/// Startup-folder items (`shell:startup` and `shell:common startup`) are
/// plain `.lnk`/`.exe` files; their approved state lives under
/// `StartupApproved\StartupFolder` (HKCU per-user, HKLM machine-wide).
fn read_startup_folder_entries(out: &mut Vec<RawStartup>, _warnings: &mut Vec<String>) {
    let folders: [(String, &str); 2] = [
        (std::env::var("APPDATA").unwrap_or_default(), "user"),
        (std::env::var("PROGRAMDATA").unwrap_or_default(), "machine"),
    ];
    for (base, scope) in folders.into_iter().filter(|(base, _)| !base.is_empty()) {
        let dir = PathBuf::from(base)
            .join("Microsoft")
            .join("Windows")
            .join("Start Menu")
            .join("Programs")
            .join("Startup");
        let Ok(entries) = std::fs::read_dir(&dir) else {
            // A missing Startup folder is normal (no items); not a warning.
            continue;
        };
        for item in entries.flatten() {
            let path = item.path();
            if path.is_dir() {
                continue;
            }
            let raw_name = item.file_name();
            if is_startup_folder_metadata(&raw_name.to_string_lossy()) {
                continue;
            }
            let file_name = item.file_name();
            let name = std::path::Path::new(&file_name)
                .file_stem()
                .map(|stem| stem.to_string_lossy().into_owned())
                .unwrap_or_else(|| file_name.to_string_lossy().into_owned());
            let command = path.to_string_lossy().into_owned();
            let enabled = startup_folder_enabled(&name);
            out.push(RawStartup {
                name,
                command,
                scope: scope.to_string(),
                source_key: dir.to_string_lossy().into_owned(),
                enabled,
                entry_type: "startup_folder".to_string(),
                hidden: false,
            });
        }
    }
}

/// Approved state for a Startup-folder item name; HKCU first (Task
/// Manager writes per-user), then HKLM.
fn startup_folder_enabled(item_name: &str) -> bool {
    for root in [HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE] {
        match open_key(root, APPROVED_STARTUP_FOLDER) {
            Ok(key) => {
                let bytes = read_value_bytes(key, item_name);
                unsafe { RegCloseKey(key) };
                if let Some(bytes) = bytes {
                    return startup_approved_enabled(&bytes);
                }
            }
            Err(_) => continue,
        }
    }
    true
}

/// Resolve the enabled state from the first allowlisted `StartupApproved`
/// key that both opens and contains the value name. Absent entries mean
/// enabled, matching Task Manager's behavior.
fn startup_entry_enabled(root: HKEY, approved_paths: &[&str], name: &str) -> bool {
    for path in approved_paths {
        match open_key(root, path) {
            Ok(key) => {
                let bytes = read_value_bytes(key, name);
                unsafe { RegCloseKey(key) };
                if let Some(bytes) = bytes {
                    return startup_approved_enabled(&bytes);
                }
                // Key opened but no such value: keep checking fallback keys
                // (e.g. WOW6432Node Run may store state in Run32).
            }
            Err(_) => continue,
        }
    }
    true
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
    out.sort_by_key(|a| a.name.to_lowercase());
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

/// Read a `REG_MULTI_SZ` value as a list of strings (empty when the value
/// is absent or of another type).
fn read_value_multi_sz(key: HKEY, name: &str) -> Vec<String> {
    let name_wide = to_wide(name);
    let mut len: u32 = 0;
    let mut ty: u32 = 0;
    let rc = unsafe {
        RegQueryValueExW(
            key,
            name_wide.as_ptr(),
            null_mut(),
            &mut ty,
            null_mut(),
            &mut len,
        )
    };
    if rc != 0 || ty != REG_MULTI_SZ || len == 0 {
        return Vec::new();
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
        return Vec::new();
    }
    buf.truncate(size as usize);
    let units: Vec<u16> = buf
        .chunks_exact(2)
        .map(|c| u16::from_le_bytes([c[0], c[1]]))
        .collect();
    // The buffer is NUL-separated and double-NUL terminated.
    units
        .split(|&u| u == 0)
        .filter(|segment| !segment.is_empty())
        .map(wide_to_string)
        .filter(|s| !s.is_empty())
        .collect()
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

// ---------------------------------------------------------------------------
// Allowlisted single-key/value probes used by update-status and PATH audits.
//
// These follow the same rule as every other read here: callers may only pass
// fixed constants defined in this module family, never user input. Key-
// existence probes never read any value content.
// ---------------------------------------------------------------------------

const CBS_REBOOT_PENDING: &str =
    "SOFTWARE\\Microsoft\\Windows\\CurrentVersion\\Component Based Servicing\\RebootPending";
const WU_REBOOT_REQUIRED: &str =
    "SOFTWARE\\Microsoft\\Windows\\CurrentVersion\\WindowsUpdate\\Auto Update\\RebootRequired";
const SESSION_MANAGER: &str = "SYSTEM\\CurrentControlSet\\Control\\Session Manager";
const MACHINE_ENVIRONMENT_KEY: &str =
    "SYSTEM\\CurrentControlSet\\Control\\Session Manager\\Environment";
const USER_ENVIRONMENT_KEY: &str = "Environment";

/// True when the fixed allowlisted key can be opened (existence probe).
pub fn allowlisted_key_exists(root: HKEY, path: &str) -> bool {
    open_key(root, path)
        .map(|k| unsafe { RegCloseKey(k) })
        .is_ok()
}

/// Read one allowlisted string-ish value (`REG_SZ` / `REG_EXPAND_SZ`),
/// unexpanded.
pub fn allowlisted_string_value(root: HKEY, key_path: &str, value_name: &str) -> Option<String> {
    let key = open_key(root, key_path).ok()?;
    let out = read_value_string(key, value_name);
    unsafe { RegCloseKey(key) };
    out
}

/// True when an allowlisted value exists and carries non-empty content.
pub fn allowlisted_value_present(root: HKEY, key_path: &str, value_name: &str) -> bool {
    allowlisted_string_value(root, key_path, value_name)
        .map(|v| !v.trim().is_empty())
        .unwrap_or(false)
}

/// The three standard pending-reboot markers, evaluated read-only.
/// Returns the list of fired signal names plus whether any fired.
pub fn pending_reboot_signals() -> Vec<String> {
    let mut signals = Vec::new();
    if allowlisted_key_exists(HKEY_LOCAL_MACHINE, CBS_REBOOT_PENDING) {
        signals.push("component_based_servicing_reboot_pending".to_string());
    }
    if allowlisted_key_exists(HKEY_LOCAL_MACHINE, WU_REBOOT_REQUIRED) {
        signals.push("windows_update_reboot_required".to_string());
    }
    if allowlisted_value_present(
        HKEY_LOCAL_MACHINE,
        SESSION_MANAGER,
        "PendingFileRenameOperations",
    ) {
        signals.push("pending_file_rename_operations".to_string());
    }
    signals
}

/// Machine-wide `Path` (`HKLM\...\Session Manager\Environment`), unexpanded.
pub fn machine_path_raw() -> Option<String> {
    allowlisted_string_value(HKEY_LOCAL_MACHINE, MACHINE_ENVIRONMENT_KEY, "Path")
}

/// Per-user `Path` (`HKCU\Environment`), unexpanded.
pub fn user_path_raw() -> Option<String> {
    allowlisted_string_value(HKEY_CURRENT_USER, USER_ENVIRONMENT_KEY, "Path")
}

/// Startup programs only; open-key warnings are collected but dropped here
/// (the caller reports them through its own channel when it has one).
pub fn startup_programs() -> Vec<StartupProgram> {
    let mut warnings = Vec::new();
    read_startup_programs(&mut warnings)
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
    fn winlogon_command_splits_on_commas_and_drops_empties() {
        assert_eq!(
            split_winlogon_command("C:\\Windows\\system32\\userinit.exe,"),
            vec!["C:\\Windows\\system32\\userinit.exe"]
        );
        assert_eq!(
            split_winlogon_command("explorer.exe , progman.exe"),
            vec!["explorer.exe", "progman.exe"]
        );
        assert!(split_winlogon_command("").is_empty());
    }

    #[test]
    fn startup_folder_metadata_files_are_skipped() {
        assert!(is_startup_folder_metadata("desktop.ini"));
        assert!(is_startup_folder_metadata("DESKTOP.INI"));
        assert!(is_startup_folder_metadata(".hidden"));
        // Real autostart items are never metadata.
        assert!(!is_startup_folder_metadata("clock2.lnk"));
        assert!(!is_startup_folder_metadata("tool.exe"));
    }

    #[test]
    fn run_sources_cover_the_five_run_keys_with_hidden_flags() {
        let paths: Vec<&str> = RUN_SOURCES.iter().map(|s| s.key_path).collect();
        assert_eq!(paths.len(), 5);
        assert!(paths.iter().any(|p| p.contains("WOW6432Node")));
        // RunOnce entries are hidden from Task Manager; Run entries are not.
        for src in RUN_SOURCES {
            assert_eq!(
                src.hidden,
                src.entry_type == "run_once",
                "{} hidden flag wrong",
                src.key_path
            );
            assert!(
                !src.approved.is_empty(),
                "{} has no approved key",
                src.key_path
            );
        }
        // The WOW64 Run entry falls back to the native Run32 approved key.
        let wow64 = RUN_SOURCES
            .iter()
            .find(|s| s.key_path.contains("WOW6432Node"))
            .unwrap();
        assert_eq!(wow64.approved.len(), 2);
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

/// Live Windows regression tests (opt-in): `WINKIT_LIVE_WINDOWS=1 cargo test
/// --features live-windows`. Guards the real-registry `UBR` read: it is a
/// `REG_DWORD`, and reading it through the string path yields garbage.
#[cfg(all(test, feature = "live-windows"))]
mod live_windows {
    use super::*;

    fn live_enabled() -> bool {
        std::env::var("WINKIT_LIVE_WINDOWS")
            .map(|v| v == "1")
            .unwrap_or(false)
    }

    #[test]
    fn ubr_reads_as_decimal_dword() {
        if !live_enabled() {
            eprintln!("SKIP: live diagnostic harness not enabled; run with WINKIT_LIVE_WINDOWS=1");
            return;
        }
        let mut warnings = Vec::new();
        let identity = read_system_identity(&mut warnings);
        let ubr = identity
            .ubr
            .expect("this Windows install reports a UBR revision");
        assert!(
            ubr.chars().all(|c| c.is_ascii_digit()),
            "UBR must be a decimal revision, got {ubr:?}"
        );
        assert!(warnings.is_empty(), "no warnings expected: {warnings:?}");
    }
}
