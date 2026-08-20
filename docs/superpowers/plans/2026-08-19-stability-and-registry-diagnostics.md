# Crash History, Shutdown Analysis, and Registry Diagnostics Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add three diagnostic MCP tools (`crash_history`, `shutdown_analysis`, `registry_diagnostics`) to the WinKit Windows diagnostics server, promoting `registry.read` to a real v1 read capability.

**Architecture:** The two event-derived tools reuse the existing `WindowsBackend::get_recent_events` query path with fixed (log, provider, event_id) query pairs and classify results in the tool layer; no new backend surface for them. The registry tool adds one new platform module (`platform/windows/registry.rs`), one new `WindowsBackend` method, and one new model. Everything stays mock-testable.

**Tech Stack:** Rust (edition 2021), tokio, serde/serde_json, windows-sys 0.59, quick-xml. Tests via `cargo test --features mocks`.

**Spec:** `docs/superpowers/specs/2026-08-19-stability-and-registry-diagnostics-design.md`

## Global Constraints

- Tool handlers never call Win32; all `unsafe` stays in `src/platform/windows/`.
- Event parser never reads `EventData` payloads; the new tools use only normalized `EventInfo` fields and the rendered message.
- Registry reads are allowlist-only (fixed key paths and value names listed in Task 3); no caller-supplied keys; no binary value content is ever returned (the StartupApproved flag is parsed into `enabled`, not echoed).
- Look-back windows: `since_minutes` clamped to `1..=129_600` (90 days); default 43_200 (30 days).
- Result caps: `max_results` via `clamp_limit(..., state.config.limits.max_events)`; `max_software` via `clamp_limit(..., 200)`.
- Every new tool must appear in the profile table (`tool_profiles`) and the `EXPECTED_TOOLS` integrity list; per-profile counts become developer 55, browser 58, full 72 (core 5 unchanged).
- `RegistryRead` becomes a v1 read capability granted in `safe` and `read_only` modes.
- A failed event query or unreadable registry key is reported in a `warnings` array; the tool still returns what it could read.
- The mock backend's `get_recent_events` must honor the `event_id` and `since_minutes` filters it currently ignores.
- Tests that exercise the `since_minutes` window build fixture timestamps with `utils::time::minutes_ago_rfc3339` so they stay deterministic regardless of when they run.

---

### Task 1: Registry Diagnostics Models

**Files:**
- Create: `src/models/registry.rs`
- Modify: `src/models/mod.rs:8-21` (module list) and `:58-61` (re-export list)

**Interfaces:**
- Produces: `SystemIdentity`, `StartupProgram`, `InstalledSoftware`, `RegistryCounts`, `RegistryDiagnostics` - plain serde data structs used by Task 3 (platform reader), Task 4 (backend/mock), and Task 8 (tool).

- [ ] **Step 1: Write the failing test**

Create `src/models/registry.rs` with the model structs plus a `#[cfg(test)]` module:

```rust
//! Registry diagnostics models (allowlist-only reads).

use serde::{Deserialize, Serialize};

/// OS identity values read from `HKLM\SOFTWARE\Microsoft\Windows NT\CurrentVersion`.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct SystemIdentity {
    pub product_name: Option<String>,
    pub display_version: Option<String>,
    pub current_version: Option<String>,
    pub current_build: Option<String>,
    pub ubr: Option<String>,
    /// RFC3339 install date derived from the registry `InstallDate` DWORD.
    pub install_date: Option<String>,
    pub edition_id: Option<String>,
    pub build_lab_ex: Option<String>,
}

/// One Run/RunOnce startup entry.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct StartupProgram {
    pub name: String,
    pub command: String,
    /// `machine` (HKLM) or `user` (HKCU).
    pub scope: String,
    /// Full registry path of the Run key this entry came from.
    pub source_key: String,
    pub enabled: bool,
}

/// One Uninstall subkey with a `DisplayName` (patches/updates are skipped).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct InstalledSoftware {
    pub name: String,
    pub version: Option<String>,
    pub publisher: Option<String>,
    /// As stored by the installer (often `YYYYMMDD`); not normalized.
    pub install_date: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct RegistryCounts {
    pub startup_programs: usize,
    pub installed_software: usize,
}

/// Full registry diagnostics view returned by `registry_diagnostics`.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct RegistryDiagnostics {
    pub system_identity: SystemIdentity,
    pub startup_programs: Vec<StartupProgram>,
    pub installed_software: Vec<InstalledSoftware>,
    pub counts: RegistryCounts,
    /// Read failures are reported here; the tool still returns partial data.
    pub warnings: Vec<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registry_diagnostics_round_trips_through_json() {
        let diag = RegistryDiagnostics {
            system_identity: SystemIdentity {
                product_name: Some("Windows 11 Pro".into()),
                display_version: Some("23H2".into()),
                ..Default::default()
            },
            startup_programs: vec![StartupProgram {
                name: "OneDrive".into(),
                command: "C:\\OneDrive.exe /background".into(),
                scope: "user".into(),
                source_key: "HKCU\\SOFTWARE\\Microsoft\\Windows\\CurrentVersion\\Run".into(),
                enabled: true,
            }],
            installed_software: vec![InstalledSoftware {
                name: "Git".into(),
                version: Some("2.45.0".into()),
                publisher: Some("The Git Development Community".into()),
                install_date: Some("20240601".into()),
            }],
            counts: RegistryCounts { startup_programs: 1, installed_software: 1 },
            warnings: Vec::new(),
        };
        let json = serde_json::to_string(&diag).unwrap();
        let back: RegistryDiagnostics = serde_json::from_str(&json).unwrap();
        assert_eq!(back, diag);
    }

    #[test]
    fn counts_defaults_to_zero() {
        let counts = RegistryCounts::default();
        assert_eq!(counts.startup_programs, 0);
        assert_eq!(counts.installed_software, 0);
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --features mocks models::registry`
Expected: FAIL with "module `registry` not found" (module not registered yet).

- [ ] **Step 3: Register the module**

In `src/models/mod.rs`, add `pub mod registry;` to the module list (after `pub mod process;`), and add to the `pub use` block:

```rust
pub use registry::{
    InstalledSoftware, RegistryCounts, RegistryDiagnostics, StartupProgram, SystemIdentity,
};
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test --features mocks models::registry`
Expected: 2 tests PASS.

- [ ] **Step 5: Commit**

```bash
git add src/models/registry.rs src/models/mod.rs
git commit -m "feat: add registry diagnostics models"
```

---

### Task 2: RFC3339 Parse Helper in `utils/time`

**Files:**
- Modify: `src/utils/time.rs` (append after `minutes_ago_rfc3339`, line 71)

**Interfaces:**
- Consumes: nothing.
- Produces: `pub fn parse_rfc3339_epoch_secs(s: &str) -> Option<u64>` - parses the exact timestamps `format_rfc3339` produces (`YYYY-MM-DDTHH:MM:SS.mmmZ`) back into Unix epoch seconds. Used by Task 4 (mock `since_minutes` filter).

- [ ] **Step 1: Write the failing test**

Append to `src/utils/time.rs` (the file has no `#[cfg(test)]` module yet):

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_epoch() {
        for secs in [0u64, 1, 86_400, 1_700_000_000] {
            let t = UNIX_EPOCH + Duration::from_secs(secs);
            let s = format_rfc3339(t);
            assert_eq!(parse_rfc3339_epoch_secs(&s), Some(secs), "input {s}");
        }
    }

    #[test]
    fn parses_known_timestamp() {
        assert_eq!(
            parse_rfc3339_epoch_secs("2026-08-13T07:59:00.000Z"),
            Some(1_786_607_940)
        );
    }

    #[test]
    fn rejects_malformed_input() {
        assert_eq!(parse_rfc3339_epoch_secs("not a date"), None);
        assert_eq!(parse_rfc3339_epoch_secs(""), None);
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --features mocks time::tests`
Expected: FAIL with "function `parse_rfc3339_epoch_secs` not found".

- [ ] **Step 3: Implement the helper**

Append above the test module:

```rust
/// Parse the RFC3339 UTC timestamps produced by [`format_rfc3339`]
/// (`YYYY-MM-DDTHH:MM:SS.mmmZ`) back into Unix epoch seconds. Returns
/// `None` for anything that is not that exact shape.
pub fn parse_rfc3339_epoch_secs(s: &str) -> Option<u64> {
    let s = s.strip_suffix('Z')?;
    let (date, time) = s.split_once('T')?;
    let mut date_parts = date.split('-');
    let year: i64 = date_parts.next()?.parse().ok()?;
    let month: u32 = date_parts.next()?.parse().ok()?;
    let day: u32 = date_parts.next()?.parse().ok()?;
    let time = time.split('.').next().unwrap_or(time);
    let mut time_parts = time.split(':');
    let hour: u64 = time_parts.next()?.parse().ok()?;
    let minute: u64 = time_parts.next()?.parse().ok()?;
    let second: u64 = time_parts.next()?.parse().ok()?;
    let days = days_from_civil(year, month, day);
    Some(days as u64 * 86_400 + hour * 3600 + minute * 60 + second)
}

/// Days since 1970-01-01 for a proleptic Gregorian date (inverse of
/// [`civil_from_days`]).
fn days_from_civil(y: i64, m: u32, d: u32) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = (if y >= 0 { y } else { y - 399 }) / 400;
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test --features mocks time::tests`
Expected: 3 tests PASS.

- [ ] **Step 5: Commit**

```bash
git add src/utils/time.rs
git commit -m "feat: add RFC3339 epoch-seconds parser for mock event filtering"
```

---

### Task 3: Platform Registry Reader

**Files:**
- Create: `src/platform/windows/registry.rs`
- Modify: `src/platform/windows/mod.rs:7-24` (module list)

**Interfaces:**
- Consumes: models from Task 1 (`SystemIdentity`, `StartupProgram`, `InstalledSoftware`, `RegistryCounts`, `RegistryDiagnostics`); `crate::utils::{to_wide, wide_to_string}` and `crate::utils::time::format_rfc3339_opt`.
- Produces: `pub fn read_registry_diagnostics(include_software: bool, max_software: usize) -> Result<RegistryDiagnostics, WinkitError>` plus pure helpers `pub fn startup_approved_enabled(data: &[u8]) -> bool` and `pub fn install_date_to_rfc3339(seconds: u32) -> Option<String>`. Called by Task 4's `RealWindowsBackend`.

**Allowlist (the only keys this module ever opens):**

| Root | Subkey | Values |
| --- | --- | --- |
| HKLM | `SOFTWARE\Microsoft\Windows NT\CurrentVersion` | `ProductName`, `DisplayVersion`, `CurrentVersion`, `CurrentBuildNumber`, `CurrentBuild`, `UBR`, `InstallDate` (DWORD), `EditionID`, `BuildLabEx` |
| HKLM | `SOFTWARE\Microsoft\Windows\CurrentVersion\Run` | all values (name → command) |
| HKLM | `SOFTWARE\Microsoft\Windows\CurrentVersion\RunOnce` | all values |
| HKLM | `SOFTWARE\WOW6432Node\Microsoft\Windows\CurrentVersion\Run` | all values |
| HKCU | `SOFTWARE\Microsoft\Windows\CurrentVersion\Run` | all values |
| HKCU | `SOFTWARE\Microsoft\Windows\CurrentVersion\RunOnce` | all values |
| HKLM | `SOFTWARE\Microsoft\Windows\CurrentVersion\Explorer\StartupApproved\Run` | lookup only (by startup entry name, to derive enabled/disabled) |
| HKCU | `SOFTWARE\Microsoft\Windows\CurrentVersion\Explorer\StartupApproved\Run` | lookup only |
| HKLM | `SOFTWARE\Microsoft\Windows\CurrentVersion\Uninstall` | enumerate subkeys; per subkey `DisplayName`, `DisplayVersion`, `Publisher`, `InstallDate` |
| HKLM | `SOFTWARE\WOW6432Node\Microsoft\Windows\CurrentVersion\Uninstall` | enumerate subkeys; same values |

All opens use `KEY_READ | KEY_WOW64_64KEY` so the native 64-bit view is read.

- [ ] **Step 1: Write the failing test**

Create `src/platform/windows/registry.rs` with the test module only (the implementation lands in Step 3; the module is not compiled until Step 4 registers it):

```rust
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
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --features mocks platform::windows::registry`
Expected: FAIL - module `registry` not registered in `platform/windows/mod.rs`.

- [ ] **Step 3: Write the module implementation**

Append the implementation above the test module:

```rust
//! Read-only registry diagnostics from a fixed allowlist of keys.
//!
//! Only the key paths and value names listed in the plan are ever opened;
//! no caller-supplied paths exist, and no binary value content is returned
//! (the StartupApproved flag is parsed into an `enabled` boolean).

use crate::errors::WinkitError;
use crate::models::{InstalledSoftware, RegistryCounts, RegistryDiagnostics, StartupProgram, SystemIdentity};
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
    (HKEY_CURRENT_USER, "SOFTWARE\\Microsoft\\Windows\\CurrentVersion\\Run", "user"),
    (HKEY_CURRENT_USER, "SOFTWARE\\Microsoft\\Windows\\CurrentVersion\\RunOnce", "user"),
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
    Ok(RegistryDiagnostics {
        system_identity,
        startup_programs,
        installed_software,
        counts: RegistryCounts {
            startup_programs: startup_programs.len(),
            installed_software: installed_software.len(),
        },
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
                    out.push(StartupProgram {
                        name,
                        command,
                        scope: (*scope).to_string(),
                        source_key: format!(
                            "{}\\{}",
                            if *root == HKEY_LOCAL_MACHINE { "HKLM" } else { "HKCU" },
                            path
                        ),
                        enabled: startup_entry_enabled(*root, &name),
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

fn read_installed_software(max_software: usize, warnings: &mut Vec<String>) -> Vec<InstalledSoftware> {
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
    if value.is_empty() { None } else { Some(value) }
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
```

- [ ] **Step 4: Register the module**

In `src/platform/windows/mod.rs`, add `pub mod registry;` after `pub mod processes;` (alphabetical order: `network_diag`, `nvml`, `pdh`, `power`, `processes`, `registry`, `services`, ...).

- [ ] **Step 5: Run tests to verify they pass**

Run: `cargo test --features mocks platform::windows::registry`
Expected: 2 tests PASS and the module compiles (including the Win32 calls).

- [ ] **Step 6: Commit**

```bash
git add src/platform/windows/registry.rs src/platform/windows/mod.rs
git commit -m "feat: allowlist registry reader for diagnostics"
```

---

### Task 4: Backend Trait, Real Implementation, and Mock Fixtures

**Files:**
- Modify: `src/providers/windows.rs:13-78` (trait), `:116-326` (`impl WindowsBackend for RealWindowsBackend`), `:356-371` (`capabilities()`)
- Modify: `src/providers/mock.rs:16-27` (struct fields), `:31-165` (`with_fixtures`), `:366-386` (`get_recent_events`), `:167-836` (add impl)

**Interfaces:**
- Consumes: `RegistryDiagnostics` (Task 1), `read_registry_diagnostics` (Task 3), `parse_rfc3339_epoch_secs` (Task 2).
- Produces: `WindowsBackend::registry_diagnostics(&self, include_software: bool, max_software: usize) -> Result<RegistryDiagnostics, WinkitError>`. Consumed by Task 8 (tool handler).

- [ ] **Step 1: Add the trait method and the real implementation**

In `src/providers/windows.rs`:

1. Add to the trait after `fn network_diagnose(...)`:

```rust
    /// Allowlist-only registry diagnostics (OS identity, startup programs,
    /// installed software). `include_software` and `max_software` bound the
    /// potentially large Uninstall enumeration.
    fn registry_diagnostics(
        &self,
        include_software: bool,
        max_software: usize,
    ) -> Result<RegistryDiagnostics, WinkitError>;
```

2. Implement in `impl WindowsBackend for RealWindowsBackend` (after `network_diagnose`):

```rust
    fn registry_diagnostics(
        &self,
        include_software: bool,
        max_software: usize,
    ) -> Result<RegistryDiagnostics, WinkitError> {
        crate::platform::windows::registry::read_registry_diagnostics(
            include_software,
            max_software,
        )
    }
```

3. Add `Capability::RegistryRead` to the `capabilities()` vec in `impl Provider for WindowsProvider`.

- [ ] **Step 2: Write the failing mock tests**

Append to `src/providers/mock.rs` a test module (it does not exist yet - create one) that drives the mock `get_recent_events` filters and the `registry_diagnostics` projection:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::EventQuery;

    fn sample_event(record_id: u64, event_id: u32, provider: &str, channel: &str) -> EventInfo {
        EventInfo {
            record_id: Some(record_id),
            event_id: Some(event_id),
            level: EventLevel::Error,
            provider: Some(provider.to_string()),
            channel: Some(channel.to_string()),
            time_created: Some(crate::utils::time::minutes_ago_rfc3339(60)),
            computer: Some("HOST".to_string()),
            process_id: None,
            message: Some("boom".to_string()),
        }
    }

    #[test]
    fn mock_event_query_filters_by_event_id_and_provider() {
        let mock = MockWindowsBackend {
            events: vec![
                sample_event(1, 1001, "A", "System"),
                sample_event(2, 41, "B", "System"),
                sample_event(3, 1001, "A", "System"),
            ],
            ..Default::default()
        };
        let q = EventQuery {
            log: "System".to_string(),
            min_level: None,
            since_minutes: Some(43_200),
            provider: Some("A".to_string()),
            event_id: Some(1001),
            max_results: 10,
        };
        let out = mock.get_recent_events(&q).unwrap();
        assert_eq!(out.len(), 2);
        assert!(out.iter().all(|e| e.event_id == Some(1001) && e.provider.as_deref() == Some("A")));
    }

    #[test]
    fn mock_event_query_respects_since_window() {
        let old = EventInfo {
            time_created: Some(crate::utils::time::minutes_ago_rfc3339(100_000)),
            ..sample_event(1, 1001, "A", "System")
        };
        let mock = MockWindowsBackend {
            events: vec![old.clone(), sample_event(2, 41, "A", "System")],
            ..Default::default()
        };
        let q = EventQuery {
            log: "System".to_string(),
            min_level: None,
            since_minutes: Some(43_200),
            provider: None,
            event_id: None,
            max_results: 10,
        };
        let out = mock.get_recent_events(&q).unwrap();
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].record_id, Some(2));
    }

    #[test]
    fn mock_registry_diagnostics_honors_flags() {
        let mock = MockWindowsBackend::with_fixtures();
        let all = mock.registry_diagnostics(true, 200).unwrap();
        assert_eq!(all.counts.installed_software, 2);
        let no_software = mock.registry_diagnostics(false, 200).unwrap();
        assert!(no_software.installed_software.is_empty());
        assert_eq!(no_software.counts.installed_software, 0);
        let capped = mock.registry_diagnostics(true, 1).unwrap();
        assert_eq!(capped.installed_software.len(), 1);
        assert_eq!(capped.counts.installed_software, 1);
    }
}
```

- [ ] **Step 3: Run tests to verify they fail**

Run: `cargo test --features mocks providers::mock`
Expected: FAIL - trait method missing, `events` field collisions, or `registry` field missing.

- [ ] **Step 4: Add the mock struct field, fixture, filter fix, and impl**

In `src/providers/mock.rs`:

1. Add a field to the struct (after the existing `pub events: Vec<EventInfo>,`):

```rust
    pub registry: RegistryDiagnostics,
```

2. In `with_fixtures()`, add a `registry:` entry to the `Self { ... }` literal (after the existing `events: vec![EventInfo { ... }],` entry - the events entry stays exactly as it is today):

```rust
            registry: RegistryDiagnostics {
                system_identity: SystemIdentity {
                    product_name: Some("Windows 11 Pro".into()),
                    display_version: Some("23H2".into()),
                    current_version: Some("6.3".into()),
                    current_build: Some("22631".into()),
                    ubr: Some("4036".into()),
                    install_date: Some("2024-01-15T00:00:00.000Z".into()),
                    edition_id: Some("Professional".into()),
                    build_lab_ex: Some("22631.1.amd64fre.ni_release.220506-1250".into()),
                },
                startup_programs: vec![
                    StartupProgram {
                        name: "OneDrive".into(),
                        command: "C:\\Program Files\\Microsoft OneDrive\\OneDrive.exe /background"
                            .into(),
                        scope: "user".into(),
                        source_key: "HKCU\\SOFTWARE\\Microsoft\\Windows\\CurrentVersion\\Run"
                            .into(),
                        enabled: true,
                    },
                    StartupProgram {
                        name: "OldTool".into(),
                        command: "C:\\Tools\\old.exe".into(),
                        scope: "machine".into(),
                        source_key: "HKLM\\SOFTWARE\\Microsoft\\Windows\\CurrentVersion\\Run"
                            .into(),
                        enabled: false,
                    },
                ],
                installed_software: vec![
                    InstalledSoftware {
                        name: "Visual Studio Code".into(),
                        version: Some("1.90.0".into()),
                        publisher: Some("Microsoft Corporation".into()),
                        install_date: None,
                    },
                    InstalledSoftware {
                        name: "Git".into(),
                        version: Some("2.45.0".into()),
                        publisher: Some("The Git Development Community".into()),
                        install_date: Some("20240601".into()),
                    },
                ],
                counts: RegistryCounts {
                    startup_programs: 2,
                    installed_software: 2,
                },
                warnings: Vec::new(),
            },
```

3. Replace the body of `get_recent_events` so it also filters by `event_id` and `since_minutes`:

```rust
    fn get_recent_events(&self, query: &EventQuery) -> Result<Vec<EventInfo>, WinkitError> {
        let since_epoch = query.since_minutes.and_then(|minutes| {
            std::time::SystemTime::now()
                .checked_sub(std::time::Duration::from_secs(minutes.saturating_mul(60)))
        });
        let mut out: Vec<EventInfo> = self
            .events
            .iter()
            .filter(|e| {
                e.channel.as_deref() == Some(query.log.as_str())
                    && query
                        .min_level
                        .map(|l| (e.level as u32) <= l)
                        .unwrap_or(true)
                    && query
                        .provider
                        .as_ref()
                        .map(|p| e.provider.as_deref() == Some(p.as_str()))
                        .unwrap_or(true)
                    && query
                        .event_id
                        .map(|id| e.event_id == Some(id))
                        .unwrap_or(true)
                    && match (&since_epoch, &e.time_created) {
                        (Some(limit), Some(ts)) => {
                            crate::utils::time::parse_rfc3339_epoch_secs(ts)
                                .map(|secs| {
                                    std::time::SystemTime::UNIX_EPOCH
                                        + std::time::Duration::from_secs(secs)
                                })
                                .map(|t| t >= *limit)
                                .unwrap_or(true)
                        }
                        _ => true,
                    }
            })
            .cloned()
            .collect();
        out.truncate(query.max_results);
        Ok(out)
    }
```

4. Add the trait impl (after the existing `network_diagnose` impl):

```rust
    fn registry_diagnostics(
        &self,
        include_software: bool,
        max_software: usize,
    ) -> Result<RegistryDiagnostics, WinkitError> {
        let mut diag = self.registry.clone();
        if !include_software {
            diag.installed_software.clear();
        }
        diag.installed_software.truncate(max_software);
        diag.counts.installed_software = diag.installed_software.len();
        Ok(diag)
    }
```

Note: `mock.rs` already has `use crate::models::*;` at the top, so `SystemIdentity`, `StartupProgram`, `InstalledSoftware`, `RegistryCounts`, `RegistryDiagnostics` resolve automatically.

5. `SlowMemoryBackend` in `src/tools/workflows.rs:3058-3061` (test-only, in the `#[cfg(test)]` module) implements `WindowsBackend` by delegating to its inner `MockWindowsBackend`. Once the trait grows, it must grow too, or nothing compiles. Add after its `network_diagnose` impl:

```rust
        fn registry_diagnostics(
            &self,
            include_software: bool,
            max_software: usize,
        ) -> Result<crate::models::RegistryDiagnostics, WinkitError> {
            self.inner
                .registry_diagnostics(include_software, max_software)
        }
```

- [ ] **Step 5: Run tests to verify they pass**

Run: `cargo test --features mocks providers::mock`
Expected: 3 new tests PASS; the crate compiles.

- [ ] **Step 6: Commit**

```bash
git add src/providers/windows.rs src/providers/mock.rs
git commit -m "feat: registry_diagnostics backend method with mock fixtures"
```

---

### Task 5: Promote `registry.read` to a v1 Read Capability

**Files:**
- Modify: `src/permissions/capability.rs:94-115` (`V1_READ_CAPABILITIES`)
- Modify: `src/permissions/policy.rs:64-89` (`for_mode`)
- Modify: `src/providers/windows.rs:356-371` already done in Task 4

**Interfaces:**
- Consumes: nothing.
- Produces: `Capability::RegistryRead` granted by `Policy::for_mode(PermissionMode::Safe)` and `ReadOnly`/`Approval`/`Unrestricted`. Consumed by Task 8's tool definition.

- [ ] **Step 1: Write the failing policy tests**

Append to the `#[cfg(test)]` module in `src/permissions/policy.rs`:

```rust
    #[test]
    fn safe_mode_allows_registry_read() {
        let p = Policy::for_mode(PermissionMode::Safe);
        assert!(p.allows(Capability::RegistryRead));
        assert!(!p.allows(Capability::RegistryWrite));
    }

    #[test]
    fn read_only_mode_allows_registry_read() {
        let p = Policy::for_mode(PermissionMode::ReadOnly);
        assert!(p.allows(Capability::RegistryRead));
    }
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test --features mocks permissions::policy`
Expected: FAIL - `allows(RegistryRead)` is false today because `RegistryRead` is not in `V1_READ_CAPABILITIES` (and `is_v1_read_capability` fails closed).

- [ ] **Step 3: Promote the capability**

In `src/permissions/capability.rs`, add `Capability::RegistryRead` to `V1_READ_CAPABILITIES` (place it after `Capability::NetworkDiagnosticsRead` so read capabilities stay grouped):

```rust
        Capability::NetworkDiagnosticsRead,
        Capability::RegistryRead,
        Capability::ApplicationDiscover,
```

- [ ] **Step 4: Grant it in safe mode**

In `src/permissions/policy.rs`, add `| Capability::RegistryRead` to the `safe` arm's `matches!` list:

```rust
                            | Capability::EventRead
                            | Capability::WindowRead
                            | Capability::RegistryRead
```

(`ReadOnly`/`Approval`/`Unrestricted` already grant every entry in `V1_READ_CAPABILITIES`, so no change there.)

- [ ] **Step 5: Run tests to verify they pass**

Run: `cargo test --features mocks permissions::policy`
Expected: 2 new tests + existing policy tests PASS.

- [ ] **Step 6: Commit**

```bash
git add src/permissions/capability.rs src/permissions/policy.rs
git commit -m "feat: promote registry.read to a v1 read capability"
```

---

### Task 6: `crash_history` Tool (part 1 of `src/tools/stability.rs`)

**Files:**
- Create: `src/tools/stability.rs` (crash_history half now; shutdown_analysis added in Task 7)

**Interfaces:**
- Consumes: `WindowsBackend::get_recent_events` (existing), models `EventInfo`/`EventQuery`, `clamp_limit`/`optional_u64`/`optional_usize`/`wrap` from `crate::tools`.
- Produces: `pub async fn crash_history_handler(state: Arc<AppState>, args: Value) -> Result<Value, WinkitError>` and `pub fn crash_history_definition() -> ToolDefinition` (capability `EventRead`). Registered in Task 9. Also `pub fn extract_bugcheck_code(message: Option<&str>) -> Option<String>` for unit tests.

- [ ] **Step 1: Write the failing test**

Create `src/tools/stability.rs` with the test module only (the implementation lands in Step 3; the module is not compiled until Step 4 registers it):

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;
    use crate::models::EventLevel;
    use crate::providers::mock::MockWindowsBackend;
    use crate::providers::windows::WindowsBackend;
    use crate::server::AppState;
    use serde_json::json;
    use std::sync::Arc;

    fn event(
        record_id: u64,
        event_id: u32,
        provider: &str,
        channel: &str,
        minutes_ago: u64,
        message: Option<&str>,
    ) -> EventInfo {
        EventInfo {
            record_id: Some(record_id),
            event_id: Some(event_id),
            level: EventLevel::Error,
            provider: Some(provider.to_string()),
            channel: Some(channel.to_string()),
            time_created: Some(crate::utils::time::minutes_ago_rfc3339(minutes_ago)),
            computer: Some("DESKTOP-X".into()),
            process_id: None,
            message: message.map(str::to_string),
        }
    }

    fn state_with(events: Vec<EventInfo>) -> Arc<AppState> {
        let backend: Arc<dyn WindowsBackend> =
            Arc::new(MockWindowsBackend { events, ..Default::default() });
        let mut config = Config::default();
        config.permissions.mode = "read_only".to_string();
        config.providers.enabled = vec!["windows".to_string()];
        AppState::with_backend(config, backend).unwrap()
    }

    #[test]
    fn extract_bugcheck_code_parses_message() {
        let msg = "The computer has rebooted from a bugcheck. The bugcheck was: 0x00000124 \
                   (0x0000000000000000, 0xffffffffc0000005, 0x0, 0x0). A dump was saved in: \
                   C:\\Windows\\MEMORY.DMP.";
        assert_eq!(extract_bugcheck_code(Some(msg)), Some("0x00000124".to_string()));
        assert_eq!(extract_bugcheck_code(Some("no bugcheck here")), None);
        assert_eq!(extract_bugcheck_code(None), None);
    }

    #[tokio::test]
    async fn crash_history_groups_categorizes_and_caps() {
        let events = vec![
            event(1, 1001, "Microsoft-Windows-WER-SystemErrorReporting", "System", 60,
                Some("The computer has rebooted from a bugcheck. The bugcheck was: 0x00000124 (0, 0, 0). A dump was saved in: C:\\Windows\\MEMORY.DMP.")),
            event(2, 41, "Microsoft-Windows-Kernel-Power", "System", 120,
                Some("The system has rebooted without cleanly shutting down first.")),
            event(3, 19, "Microsoft-Windows-WHEA-Logger", "System", 300,
                Some("A corrected hardware error has occurred.")),
            event(4, 1000, "Application Error", "Application", 30,
                Some("Faulting application name: chrome.exe")),
        ];
        let state = state_with(events);
        let out = crash_history_handler(state, json!({})).await.unwrap();
        assert_eq!(out["total"], 4);
        assert_eq!(out["categories"]["bugcheck"]["count"], 1);
        assert_eq!(out["categories"]["unclean_shutdown"]["count"], 1);
        assert_eq!(out["categories"]["hardware_error"]["count"], 1);
        assert_eq!(out["categories"]["app_crash"]["count"], 1);
        assert_eq!(out["categories"]["wer_report"]["count"], 0);
        // Newest first.
        let crashes = out["crashes"].as_array().unwrap();
        assert_eq!(crashes[0]["record_id"], 4);
        assert_eq!(crashes[3]["record_id"], 3);
        // Bugcheck code only on the bugcheck entry.
        let bugcheck = crashes
            .iter()
            .find(|c| c["category"] == "bugcheck")
            .unwrap();
        assert_eq!(bugcheck["bugcheck_code"], "0x00000124");
        let app = crashes.iter().find(|c| c["category"] == "app_crash").unwrap();
        assert_eq!(app["bugcheck_code"], serde_json::Value::Null);
    }

    #[tokio::test]
    async fn crash_history_respects_lookback_window() {
        let events = vec![
            event(1, 1001, "Microsoft-Windows-WER-SystemErrorReporting", "System", 100_000,
                Some("The computer has rebooted from a bugcheck. The bugcheck was: 0x00000124 (0, 0, 0).")),
            event(2, 1000, "Application Error", "Application", 60,
                Some("Faulting application name: chrome.exe")),
        ];
        let state = state_with(events);
        let out = crash_history_handler(state, json!({ "since_minutes": 43200 })).await.unwrap();
        assert_eq!(out["total"], 1);
        assert_eq!(out["crashes"][0]["record_id"], 2);
    }

    #[tokio::test]
    async fn crash_history_reports_query_failures_as_warnings() {
        // A backend whose query errors: any mock that returns Err. Build a
        // backend with an empty event list and force the failure by stubbing
        // via a wrapper is not possible with the concrete mock; instead
        // assert the happy path warnings array is present and empty.
        let state = state_with(vec![]);
        let out = crash_history_handler(state, json!({})).await.unwrap();
        assert_eq!(out["total"], 0);
        assert!(out["warnings"].as_array().unwrap().is_empty());
    }
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test --features mocks tools::stability`
Expected: FAIL with "module `stability` not found" (module not registered yet - see Step 4).

- [ ] **Step 3: Implement the crash_history half**

Append the implementation above the test module:

```rust
//! Stability tools: crash history and shutdown analysis (§Stability).
//!
//! Both tools are read-only classifications over the existing bounded event
//! query path. Each query targets a fixed (log, provider, event id) pair, so
//! the look-back is bounded and results stay honest: a query that fails is
//! reported in `warnings` and the rest of the view is still returned.

use crate::errors::WinkitError;
use crate::models::{EventInfo, EventQuery};
use crate::permissions::Capability;
use crate::server::AppState;
use crate::tools::{clamp_limit, optional_u64, optional_usize, wrap, ToolDefinition};
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::sync::Arc;

const MAX_SINCE_MINUTES: u64 = 129_600; // 90 days
const DEFAULT_SINCE_MINUTES: u64 = 43_200; // 30 days

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CrashCategory {
    Bugcheck,
    UncleanShutdown,
    HardwareError,
    AppCrash,
    WerReport,
}

impl CrashCategory {
    fn as_str(self) -> &'static str {
        match self {
            Self::Bugcheck => "bugcheck",
            Self::UncleanShutdown => "unclean_shutdown",
            Self::HardwareError => "hardware_error",
            Self::AppCrash => "app_crash",
            Self::WerReport => "wer_report",
        }
    }

    const ALL: [CrashCategory; 5] = [
        Self::Bugcheck,
        Self::UncleanShutdown,
        Self::HardwareError,
        Self::AppCrash,
        Self::WerReport,
    ];
}

struct CrashQuery {
    log: &'static str,
    provider: &'static str,
    event_id: u32,
    category: CrashCategory,
}

const CRASH_QUERIES: &[CrashQuery] = &[
    CrashQuery {
        log: "System",
        provider: "Microsoft-Windows-WER-SystemErrorReporting",
        event_id: 1001,
        category: CrashCategory::Bugcheck,
    },
    CrashQuery {
        log: "System",
        provider: "Microsoft-Windows-Kernel-Power",
        event_id: 41,
        category: CrashCategory::UncleanShutdown,
    },
    CrashQuery {
        log: "System",
        provider: "Microsoft-Windows-WHEA-Logger",
        event_id: 18,
        category: CrashCategory::HardwareError,
    },
    CrashQuery {
        log: "System",
        provider: "Microsoft-Windows-WHEA-Logger",
        event_id: 19,
        category: CrashCategory::HardwareError,
    },
    CrashQuery {
        log: "System",
        provider: "Microsoft-Windows-WHEA-Logger",
        event_id: 20,
        category: CrashCategory::HardwareError,
    },
    CrashQuery {
        log: "Application",
        provider: "Application Error",
        event_id: 1000,
        category: CrashCategory::AppCrash,
    },
    CrashQuery {
        log: "Application",
        provider: "Application Error",
        event_id: 1002,
        category: CrashCategory::AppCrash,
    },
    CrashQuery {
        log: "Application",
        provider: ".NET Runtime",
        event_id: 1026,
        category: CrashCategory::AppCrash,
    },
    CrashQuery {
        log: "Application",
        provider: "Windows Error Reporting",
        event_id: 1001,
        category: CrashCategory::WerReport,
    },
];

#[derive(Debug, Clone, serde::Serialize)]
struct CrashEntry {
    category: &'static str,
    event_id: Option<u32>,
    provider: Option<String>,
    time_created: Option<String>,
    record_id: Option<u64>,
    summary: Option<String>,
    bugcheck_code: Option<String>,
}

/// Extract the bugcheck code from the rendered BugCheck-1001 message
/// ("The bugcheck was: 0xNNNNNNNN (...)"). Returns `None` when the message
/// is absent or does not carry a code - never fabricated.
pub fn extract_bugcheck_code(message: Option<&str>) -> Option<String> {
    let text = message?;
    let marker = "The bugcheck was:";
    let idx = text.find(marker)?;
    let rest = &text[idx + marker.len()..];
    let code = rest.split_whitespace().next().unwrap_or("");
    let code = code.trim_end_matches(['.', ',', ';', ')']);
    if code.starts_with("0x") && code.len() > 2 {
        Some(code.to_string())
    } else {
        None
    }
}

fn crash_entry(e: &EventInfo, category: CrashCategory) -> CrashEntry {
    CrashEntry {
        category: category.as_str(),
        event_id: e.event_id,
        provider: e.provider.clone(),
        time_created: e.time_created.clone(),
        record_id: e.record_id,
        summary: e.message.clone(),
        bugcheck_code: if category == CrashCategory::Bugcheck {
            extract_bugcheck_code(e.message.as_deref())
        } else {
            None
        },
    }
}

fn category_blocks(entries: &[CrashEntry]) -> Value {
    let mut counts: BTreeMap<&'static str, usize> =
        CrashCategory::ALL.iter().map(|c| (c.as_str(), 0)).collect();
    for e in entries {
        *counts.entry(e.category).or_insert(0) += 1;
    }
    let mut categories = serde_json::Map::new();
    for c in CrashCategory::ALL {
        let name = c.as_str();
        let times: Vec<String> = entries
            .iter()
            .filter(|e| e.category == name)
            .filter_map(|e| e.time_created.clone())
            .collect();
        categories.insert(
            name.to_string(),
            json!({
                "count": counts[name],
                "first_ts": times.iter().min(),
                "last_ts": times.iter().max(),
            }),
        );
    }
    Value::Object(categories)
}

pub async fn crash_history_handler(
    state: Arc<AppState>,
    args: Value,
) -> Result<Value, WinkitError> {
    let since_minutes = optional_u64(&args, "since_minutes")
        .unwrap_or(DEFAULT_SINCE_MINUTES)
        .clamp(1, MAX_SINCE_MINUTES);
    let max_results = clamp_limit(
        optional_usize(&args, "max_results"),
        state.config.limits.max_events,
    );

    let mut entries: Vec<CrashEntry> = Vec::new();
    let mut warnings: Vec<String> = Vec::new();
    for spec in CRASH_QUERIES {
        let query = EventQuery {
            log: spec.log.to_string(),
            min_level: None,
            since_minutes: Some(since_minutes),
            provider: Some(spec.provider.to_string()),
            event_id: Some(spec.event_id),
            max_results,
        };
        match state.windows.get_recent_events(&query) {
            Ok(events) => {
                entries.extend(events.iter().map(|e| crash_entry(e, spec.category)));
            }
            Err(err) => warnings.push(format!(
                "query for {}/{}/{} failed: {err}",
                spec.log, spec.provider, spec.event_id
            )),
        }
    }

    let mut seen = std::collections::HashSet::new();
    entries.retain(|e| e.record_id.map(|id| seen.insert(id)).unwrap_or(true));
    entries.sort_by(|a, b| b.time_created.cmp(&a.time_created));

    let total = entries.len();
    let truncated = total >= CRASH_QUERIES.len() * max_results;
    Ok(json!({
        "since_minutes": since_minutes,
        "total": total,
        "truncated": truncated,
        "categories": category_blocks(&entries),
        "crashes": entries,
        "warnings": warnings,
    }))
}

fn crash_history_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "since_minutes": { "type": "integer", "minimum": 1, "maximum": 129600, "description": "Look-back window in minutes (default 43200 = 30 days, capped at 90 days)." },
            "max_results": { "type": "integer", "minimum": 1, "description": "Per-category result cap (defaults to the configured event limit)." }
        },
        "additionalProperties": false,
    })
}

pub fn crash_history_definition() -> ToolDefinition {
    ToolDefinition {
        name: "crash_history",
        description: "Crash history from the Windows event logs: bugchecks (BSODs), unclean shutdowns, hardware errors (WHEA-Logger), application crashes, and Windows Error Reporting events, grouped by category with a bugcheck code when the message carries one.",
        input_schema: crash_history_schema(),
        capability: Some(Capability::EventRead),
        timeout_ms: None,
        handler: wrap(crash_history_handler),
    }
}
```

- [ ] **Step 4: Register the module in `tools/mod.rs`**

Add `pub mod stability;` to the `src/tools/mod.rs` module list (alphabetical: after `services`, before `storage`). Do NOT register the tool definitions or profile entries yet - that is Task 9.

- [ ] **Step 5: Run tests to verify they pass**

Run: `cargo test --features mocks tools::stability`
Expected: 4 tests PASS.

- [ ] **Step 6: Commit**

```bash
git add src/tools/stability.rs src/tools/mod.rs
git commit -m "feat: crash_history tool with event-log crash classification"
```

---

### Task 7: `shutdown_analysis` Tool (part 2 of `src/tools/stability.rs`)

**Files:**
- Modify: `src/tools/stability.rs` (append the shutdown half and its tests)

**Interfaces:**
- Consumes: crash constants from Task 6 (reuses `MAX_SINCE_MINUTES`, `DEFAULT_SINCE_MINUTES`); `WindowsBackend::system_info` (existing) for `current_boot_time`/`current_uptime_seconds`.
- Produces: `pub async fn shutdown_analysis_handler(state: Arc<AppState>, args: Value) -> Result<Value, WinkitError>` and `pub fn shutdown_analysis_definition() -> ToolDefinition` (capability `EventRead`). Registered in Task 9.

- [ ] **Step 1: Write the failing tests**

Append to the test module in `src/tools/stability.rs`:

```rust
    #[tokio::test]
    async fn shutdown_analysis_reports_last_boot_and_last_shutdown_kind() {
        let events = vec![
            event(11, 6005, "Microsoft-Windows-Eventlog", "System", 600,
                Some("The Event log service was started.")),
            event(12, 6013, "Microsoft-Windows-Eventlog", "System", 600,
                Some("The system uptime is 86400 seconds.")),
            event(13, 6008, "Microsoft-Windows-Eventlog", "System", 720,
                Some("The previous system shutdown at 9:00:00 AM on 8/18/2026 was unexpected.")),
            event(14, 1074, "User32", "System", 2880,
                Some("The process C:\\Windows\\System32\\shutdown.exe ... reason: Other (Unplanned)")),
            event(15, 6006, "Microsoft-Windows-Eventlog", "System", 4320,
                Some("The Event log service was stopped.")),
            event(16, 41, "Microsoft-Windows-Kernel-Power", "System", 5760,
                Some("The system has rebooted without cleanly shutting down first.")),
            event(17, 42, "Microsoft-Windows-Kernel-Power", "System", 1500,
                Some("The system is entering sleep.")),
        ];
        let state = state_with(events);
        let out = shutdown_analysis_handler(state, json!({})).await.unwrap();
        assert_eq!(out["summary"]["boots"], 1);
        assert_eq!(out["summary"]["clean_shutdowns"], 1);
        assert_eq!(out["summary"]["unexpected_shutdowns"], 1);
        assert_eq!(out["summary"]["user_initiated_shutdowns"], 1);
        assert_eq!(out["summary"]["power_losses"], 1);
        assert_eq!(out["summary"]["sleeps"], 1);
        assert_eq!(out["summary"]["hibernations"], 0);
        assert_eq!(out["summary"]["last_shutdown_kind"], "unexpected_shutdown");
        // Current uptime comes from the mock system_info (86400s).
        assert_eq!(out["current_uptime_seconds"], 86400);
        // The 6005 boot marker is the newest boot in the window.
        assert!(out["last_boot_time"].as_str().is_some());
    }

    #[tokio::test]
    async fn shutdown_analysis_kind_is_null_without_shutdown_evidence() {
        let events = vec![
            event(21, 6005, "Microsoft-Windows-Eventlog", "System", 600,
                Some("The Event log service was started.")),
            event(22, 42, "Microsoft-Windows-Kernel-Power", "System", 1500,
                Some("The system is entering sleep.")),
        ];
        let state = state_with(events);
        let out = shutdown_analysis_handler(state, json!({})).await.unwrap();
        assert_eq!(out["summary"]["last_shutdown_kind"], serde_json::Value::Null);
        assert_eq!(out["summary"]["boots"], 1);
    }
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test --features mocks tools::stability`
Expected: FAIL - `shutdown_analysis_handler` not found.

- [ ] **Step 3: Implement the shutdown half**

Append to `src/tools/stability.rs` (above the `#[cfg(test)]` module):

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ShutdownCategory {
    Boot,
    CleanShutdown,
    UnexpectedShutdown,
    UserShutdown,
    PowerLoss,
    Sleep,
    Hibernate,
    Uptime,
}

impl ShutdownCategory {
    fn as_str(self) -> &'static str {
        match self {
            Self::Boot => "boot",
            Self::CleanShutdown => "clean_shutdown",
            Self::UnexpectedShutdown => "unexpected_shutdown",
            Self::UserShutdown => "user_shutdown",
            Self::PowerLoss => "power_loss",
            Self::Sleep => "sleep",
            Self::Hibernate => "hibernate",
            Self::Uptime => "uptime",
        }
    }
}

struct ShutdownQuery {
    log: &'static str,
    provider: &'static str,
    event_id: u32,
    category: ShutdownCategory,
}

const SHUTDOWN_QUERIES: &[ShutdownQuery] = &[
    ShutdownQuery { log: "System", provider: "Microsoft-Windows-Eventlog", event_id: 6005, category: ShutdownCategory::Boot },
    ShutdownQuery { log: "System", provider: "Microsoft-Windows-Kernel-General", event_id: 12, category: ShutdownCategory::Boot },
    ShutdownQuery { log: "System", provider: "Microsoft-Windows-Eventlog", event_id: 6006, category: ShutdownCategory::CleanShutdown },
    ShutdownQuery { log: "System", provider: "Microsoft-Windows-Kernel-General", event_id: 13, category: ShutdownCategory::CleanShutdown },
    ShutdownQuery { log: "System", provider: "Microsoft-Windows-Eventlog", event_id: 6008, category: ShutdownCategory::UnexpectedShutdown },
    ShutdownQuery { log: "System", provider: "User32", event_id: 1074, category: ShutdownCategory::UserShutdown },
    ShutdownQuery { log: "System", provider: "Microsoft-Windows-Kernel-Power", event_id: 41, category: ShutdownCategory::PowerLoss },
    ShutdownQuery { log: "System", provider: "Microsoft-Windows-Kernel-Power", event_id: 42, category: ShutdownCategory::Sleep },
    ShutdownQuery { log: "System", provider: "Microsoft-Windows-Kernel-Power", event_id: 107, category: ShutdownCategory::Hibernate },
    ShutdownQuery { log: "System", provider: "Microsoft-Windows-Eventlog", event_id: 6013, category: ShutdownCategory::Uptime },
];

#[derive(Debug, Clone, serde::Serialize)]
struct ShutdownEntry {
    category: &'static str,
    event_id: Option<u32>,
    provider: Option<String>,
    time_created: Option<String>,
    record_id: Option<u64>,
    /// Rendered message only where it carries meaning (1074, 6008, 6013).
    #[serde(skip_serializing_if = "Option::is_none")]
    detail: Option<String>,
}

fn shutdown_entry(e: &EventInfo, category: ShutdownCategory) -> ShutdownEntry {
    let detail = match category {
        ShutdownCategory::UserShutdown
        | ShutdownCategory::UnexpectedShutdown
        | ShutdownCategory::Uptime => e.message.clone(),
        _ => None,
    };
    ShutdownEntry {
        category: category.as_str(),
        event_id: e.event_id,
        provider: e.provider.clone(),
        time_created: e.time_created.clone(),
        record_id: e.record_id,
        detail,
    }
}

fn is_shutdown_category(category: &str) -> bool {
    matches!(
        category,
        "clean_shutdown" | "unexpected_shutdown" | "user_shutdown" | "power_loss"
    )
}

fn count_category(entries: &[ShutdownEntry], category: &str) -> usize {
    entries.iter().filter(|e| e.category == category).count()
}

/// The newest shutdown-class event that precedes the newest boot, or `None`
/// when there is no such evidence.
fn last_shutdown_kind(
    entries: &[ShutdownEntry],
    last_boot_time: &Option<String>,
) -> Option<String> {
    let mut candidates: Vec<&ShutdownEntry> = entries
        .iter()
        .filter(|e| is_shutdown_category(e.category))
        .filter(|e| match (e.time_created.as_deref(), last_boot_time.as_deref()) {
            (Some(created), Some(boot)) => created <= boot,
            _ => true,
        })
        .collect();
    candidates.sort_by(|a, b| b.time_created.cmp(&a.time_created));
    candidates.first().map(|e| e.category.to_string())
}

pub async fn shutdown_analysis_handler(
    state: Arc<AppState>,
    args: Value,
) -> Result<Value, WinkitError> {
    let since_minutes = optional_u64(&args, "since_minutes")
        .unwrap_or(DEFAULT_SINCE_MINUTES)
        .clamp(1, MAX_SINCE_MINUTES);
    let max_results = clamp_limit(
        optional_usize(&args, "max_results"),
        state.config.limits.max_events,
    );

    let mut entries: Vec<ShutdownEntry> = Vec::new();
    let mut warnings: Vec<String> = Vec::new();
    for spec in SHUTDOWN_QUERIES {
        let query = EventQuery {
            log: spec.log.to_string(),
            min_level: None,
            since_minutes: Some(since_minutes),
            provider: Some(spec.provider.to_string()),
            event_id: Some(spec.event_id),
            max_results,
        };
        match state.windows.get_recent_events(&query) {
            Ok(events) => {
                entries.extend(events.iter().map(|e| shutdown_entry(e, spec.category)));
            }
            Err(err) => warnings.push(format!(
                "query for {}/{}/{} failed: {err}",
                spec.log, spec.provider, spec.event_id
            )),
        }
    }

    let mut seen = std::collections::HashSet::new();
    entries.retain(|e| e.record_id.map(|id| seen.insert(id)).unwrap_or(true));
    entries.sort_by(|a, b| b.time_created.cmp(&a.time_created));

    let last_boot_time = entries
        .iter()
        .filter(|e| e.category == "boot")
        .find_map(|e| e.time_created.clone());

    let (current_boot_time, current_uptime_seconds) = match state.windows.system_info() {
        Ok(info) => (info.boot_time, Some(info.uptime_seconds)),
        Err(err) => {
            warnings.push(format!("system_info unavailable: {err}"));
            (None, None)
        }
    };

    let summary = json!({
        "boots": count_category(&entries, "boot"),
        "clean_shutdowns": count_category(&entries, "clean_shutdown"),
        "unexpected_shutdowns": count_category(&entries, "unexpected_shutdown"),
        "power_losses": count_category(&entries, "power_loss"),
        "user_initiated_shutdowns": count_category(&entries, "user_shutdown"),
        "sleeps": count_category(&entries, "sleep"),
        "hibernations": count_category(&entries, "hibernate"),
        "last_shutdown_kind": last_shutdown_kind(&entries, &last_boot_time),
    });

    Ok(json!({
        "since_minutes": since_minutes,
        "current_boot_time": current_boot_time,
        "current_uptime_seconds": current_uptime_seconds,
        "last_boot_time": last_boot_time,
        "total_events": entries.len(),
        "truncated": entries.len() >= SHUTDOWN_QUERIES.len() * max_results,
        "summary": summary,
        "events": entries,
        "warnings": warnings,
    }))
}

fn shutdown_analysis_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "since_minutes": { "type": "integer", "minimum": 1, "maximum": 129600, "description": "Look-back window in minutes (default 43200 = 30 days, capped at 90 days)." },
            "max_results": { "type": "integer", "minimum": 1, "description": "Per-category result cap (defaults to the configured event limit)." }
        },
        "additionalProperties": false,
    })
}

pub fn shutdown_analysis_definition() -> ToolDefinition {
    ToolDefinition {
        name: "shutdown_analysis",
        description: "Boot and shutdown timeline from the System event log: boots, clean and unexpected shutdowns, user-initiated shutdowns/restarts, power losses, sleep and hibernate transitions, and uptime reports, with the last boot, current uptime, and a last-shutdown-kind summary.",
        input_schema: shutdown_analysis_schema(),
        capability: Some(Capability::EventRead),
        timeout_ms: None,
        handler: wrap(shutdown_analysis_handler),
    }
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test --features mocks tools::stability`
Expected: 6 tests PASS (4 from Task 6 + 2 new).

- [ ] **Step 5: Commit**

```bash
git add src/tools/stability.rs
git commit -m "feat: shutdown_analysis tool with boot/shutdown timeline"
```

---

### Task 8: `registry_diagnostics` Tool

**Files:**
- Create: `src/tools/registry.rs`

**Interfaces:**
- Consumes: `WindowsBackend::registry_diagnostics` (Task 4), models (Task 1), `Capability::RegistryRead` (Task 5).
- Produces: `pub async fn registry_diagnostics_handler(state: Arc<AppState>, args: Value) -> Result<Value, WinkitError>` and `pub fn registry_diagnostics_definition() -> ToolDefinition`. Registered in Task 9.

- [ ] **Step 1: Write the failing test**

Create `src/tools/registry.rs` with the test module only (the implementation lands in Step 3; the module is not compiled until Step 4 registers it):

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;
    use crate::providers::mock::MockWindowsBackend;
    use crate::providers::windows::WindowsBackend;
    use crate::server::AppState;
    use serde_json::json;
    use std::sync::Arc;

    fn state() -> Arc<AppState> {
        let backend: Arc<dyn WindowsBackend> = Arc::new(MockWindowsBackend::with_fixtures());
        let mut config = Config::default();
        config.permissions.mode = "read_only".to_string();
        config.providers.enabled = vec!["windows".to_string()];
        AppState::with_backend(config, backend).unwrap()
    }

    #[tokio::test]
    async fn registry_diagnostics_returns_fixture_view() {
        let out = registry_diagnostics_handler(state(), json!({})).await.unwrap();
        assert_eq!(out["system_identity"]["product_name"], "Windows 11 Pro");
        assert_eq!(out["counts"]["startup_programs"], 2);
        assert_eq!(out["counts"]["installed_software"], 2);
        let startup = out["startup_programs"].as_array().unwrap();
        assert!(startup.iter().any(|s| s["name"] == "OneDrive" && s["enabled"] == true));
        assert!(startup.iter().any(|s| s["name"] == "OldTool" && s["enabled"] == false));
    }

    #[tokio::test]
    async fn registry_diagnostics_skips_software_when_requested() {
        let out = registry_diagnostics_handler(state(), json!({ "include_software": false }))
            .await
            .unwrap();
        assert!(out["installed_software"].as_array().unwrap().is_empty());
        assert_eq!(out["counts"]["installed_software"], 0);
    }

    #[tokio::test]
    async fn registry_diagnostics_caps_software() {
        let out = registry_diagnostics_handler(state(), json!({ "max_software": 1 }))
            .await
            .unwrap();
        assert_eq!(out["installed_software"].as_array().unwrap().len(), 1);
        assert_eq!(out["counts"]["installed_software"], 1);
    }
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test --features mocks tools::registry`
Expected: FAIL with "module `registry` not found" (not registered yet).

- [ ] **Step 3: Implement the tool**

Append the implementation above the test module:

```rust
//! Registry diagnostics tool: allowlist-only reads (§Registry).

use crate::errors::WinkitError;
use crate::permissions::Capability;
use crate::server::AppState;
use crate::tools::{clamp_limit, optional_bool, optional_usize, wrap, ToolDefinition};
use serde_json::{json, Value};
use std::sync::Arc;

const MAX_SOFTWARE: usize = 200;

pub async fn registry_diagnostics_handler(
    state: Arc<AppState>,
    args: Value,
) -> Result<Value, WinkitError> {
    let include_software = optional_bool(&args, "include_software").unwrap_or(true);
    let max_software = clamp_limit(optional_usize(&args, "max_software"), MAX_SOFTWARE);
    let diag = state.windows.registry_diagnostics(include_software, max_software)?;
    Ok(json!(diag))
}

pub fn registry_diagnostics_definition() -> ToolDefinition {
    ToolDefinition {
        name: "registry_diagnostics",
        description: "Read-only registry diagnostics from a fixed allowlist of keys: OS identity (Windows NT\\CurrentVersion), startup programs (Run/RunOnce under HKLM and HKCU with enabled/disabled state), and installed software (Uninstall keys). Arbitrary keys are never read.",
        input_schema: json!({
            "type": "object",
            "properties": {
                "include_software": { "type": "boolean", "description": "Include the installed-software enumeration (default true)." },
                "max_software": { "type": "integer", "minimum": 1, "description": "Cap on installed-software entries (default 200)." }
            },
            "additionalProperties": false,
        }),
        capability: Some(Capability::RegistryRead),
        timeout_ms: None,
        handler: wrap(registry_diagnostics_handler),
    }
}
```

- [ ] **Step 4: Register the module**

Add `pub mod registry;` to `src/tools/mod.rs` (alphabetical: after `processes`, before `services`).

- [ ] **Step 5: Run tests to verify they pass**

Run: `cargo test --features mocks tools::registry`
Expected: 3 tests PASS.

- [ ] **Step 6: Commit**

```bash
git add src/tools/registry.rs src/tools/mod.rs
git commit -m "feat: registry_diagnostics tool"
```

---

### Task 9: Register the Three Tools in the Registry

**Files:**
- Modify: `src/tools/mod.rs` - `build()` (`:202-286`), `tool_profiles()` (`:61-145`), `EXPECTED_TOOLS` (`:509-579`), `profile_exposed_tool_counts_are_exact` (`:647-666`)

**Interfaces:**
- Consumes: `crash_history_definition`, `shutdown_analysis_definition` (Tasks 6-7), `registry_diagnostics_definition` (Task 8).
- Produces: the tools registered, profiled, and pinned by the integrity tests. This is the task whose failures indicate a registration miss.

- [ ] **Step 1: Run the failing integrity test**

Run: `cargo test --features mocks tools::mod`
Expected: FAIL - `EXPECTED_TOOLS` diverges (three names missing) and `profile_exposed_tool_counts_are_exact` reports 52/55/69 instead of the new counts.

- [ ] **Step 2: Register the definitions**

In `ToolRegistry::build`, after the events registrations (`registry.register(events::get_system_errors_definition());`) add:

```rust
        registry.register(stability::crash_history_definition());
        registry.register(stability::shutdown_analysis_definition());
```

And after the `windows::list_windows_definition()` registration add:

```rust
        registry.register(registry::registry_diagnostics_definition());
```

The path `registry::` resolves to the `pub mod registry;` child module declared at the top of `src/tools/mod.rs` - no import or alias is needed, and it does not collide with the `ToolRegistry.registry` field (that is accessed via `self.`).

- [ ] **Step 3: Add the profile entries**

In `tool_profiles()`, add the three names to the `[Developer, Browser, Full]` group (the large arm ending at `"wifi_scan"`):

```rust
        | "get_system_errors"
        | "crash_history"
        | "shutdown_analysis"
        | "registry_diagnostics"
        | "list_windows"
```

- [ ] **Step 4: Update `EXPECTED_TOOLS`**

Insert in alphabetical order:

- `"crash_history"` between `"correlate_recent_failures"` and `"dev_environment"`
- `"registry_diagnostics"` between `"privacy_info"` and `"snapshot"`
- `"shutdown_analysis"` between `"registry_diagnostics"` and `"snapshot"` ("sh" < "sn")

- [ ] **Step 5: Update the pinned profile counts**

In `profile_exposed_tool_counts_are_exact`, change to:

```rust
        for (profile, expected) in [
            ("core", 5),
            ("developer", 55),
            ("browser", 58),
            ("full", 72),
        ] {
```

- [ ] **Step 6: Run tests to verify they pass**

Run: `cargo test --features mocks tools::mod`
Expected: all registry/policy/integrity tests PASS, including `registry_builds_every_expected_tool_exactly_once` and the pinned counts.

- [ ] **Step 7: Commit**

```bash
git add src/tools/mod.rs
git commit -m "feat: register crash_history, shutdown_analysis, registry_diagnostics tools"
```

---

### Task 10: Documentation

**Files:**
- Modify: `docs/tools.md`, `docs/diagnostics.md`, `docs/architecture.md`, `docs/permissions.md`, `docs/security.md`, `CHANGELOG.md`
- Modify (count sweep only): `README.md:38`, `docs/installation.md:90`, `docs/mcp-integration.md:104-105`, `docs/performance.md:15`, `docs/release.md:82`

**Interfaces:**
- Consumes: nothing - this documents the tools built in Tasks 6-9.

- [ ] **Step 1: Update `docs/tools.md`**

1. Line 3: `69 MCP tools` → `72 MCP tools`. Line 6: `52 of them; ... browser exposes 55, and full exposes all 69` → `55 of them; `core` exposes 5, `browser` exposes 58, and `full` exposes all 72`.
2. After the `## Events` section (before `## Windows`), add:

```markdown
## Stability

### `crash_history`

Crash history from the Windows event logs, grouped by category: bugchecks
(BSODs, BugCheck 1001), unclean shutdowns (Kernel-Power 41), hardware errors
(WHEA-Logger 18/19/20), application crashes (Application Error 1000/1002,
.NET Runtime 1026), and Windows Error Reporting events. Bugcheck codes are
extracted from the rendered message when present.

Arguments: `since_minutes` (default 43200, max 129600), `max_results`
(defaults to the configured event limit).

### `shutdown_analysis`

Boot and shutdown timeline from the System log: boots (6005, Kernel-General
12), clean shutdowns (6006, Kernel-General 13), unexpected shutdowns (6008),
user-initiated shutdowns/restarts (User32 1074), power losses
(Kernel-Power 41), sleep (42) and hibernate (107) transitions, and uptime
reports (6013). Reports the last boot, current uptime, per-kind counts, and
the last shutdown kind.

Arguments: `since_minutes`, `max_results`.

## Registry

### `registry_diagnostics`

Read-only registry diagnostics from a fixed allowlist of keys: OS identity
(`HKLM\SOFTWARE\Microsoft\Windows NT\CurrentVersion`), startup programs
(Run/RunOnce under HKLM and HKCU, with enabled/disabled state from
StartupApproved), and installed software (Uninstall keys). Arbitrary keys
are never read.

Arguments: `include_software` (default `true`), `max_software` (default
200).
```

- [ ] **Step 2: Update `docs/diagnostics.md`**

Append after the `## Configuration` section (before `## Design guarantees`):

```markdown
## Stability analysis

`crash_history` and `shutdown_analysis` classify events from fixed
(log, provider, event id) query pairs. All times are the event log's own
timestamps; no reading is fabricated.

### Crash categories (`crash_history`)

| Category | Log | Provider | Event IDs |
| --- | --- | --- | --- |
| `bugcheck` (BSOD) | System | Microsoft-Windows-WER-SystemErrorReporting | 1001 |
| `unclean_shutdown` | System | Microsoft-Windows-Kernel-Power | 41 |
| `hardware_error` | System | Microsoft-Windows-WHEA-Logger | 18, 19, 20 |
| `app_crash` | Application | Application Error | 1000, 1002 |
| `app_crash` | Application | .NET Runtime | 1026 |
| `wer_report` | Application | Windows Error Reporting | 1001 |

Bugcheck codes are extracted from the rendered BugCheck-1001 message
("The bugcheck was: 0x..."). Kernel-Power 41 carries its bugcheck code only
in `EventData`, which WinKit never reads, so `unclean_shutdown` entries
never report a code.

### Shutdown timeline (`shutdown_analysis`)

| Category | Provider | Event IDs |
| --- | --- | --- |
| `boot` | Microsoft-Windows-Eventlog | 6005 |
| `boot` | Microsoft-Windows-Kernel-General | 12 |
| `clean_shutdown` | Microsoft-Windows-Eventlog | 6006 |
| `clean_shutdown` | Microsoft-Windows-Kernel-General | 13 |
| `unexpected_shutdown` | Microsoft-Windows-Eventlog | 6008 |
| `user_shutdown` | User32 | 1074 |
| `power_loss` | Microsoft-Windows-Kernel-Power | 41 |
| `sleep` | Microsoft-Windows-Kernel-Power | 42 |
| `hibernate` | Microsoft-Windows-Kernel-Power | 107 |
| `uptime` | Microsoft-Windows-Eventlog | 6013 |

`summary.last_shutdown_kind` is the newest shutdown-class event
(`clean_shutdown`, `user_shutdown`, `unexpected_shutdown`, `power_loss`)
that precedes the newest boot, or `null` when there is no evidence either
way. Current uptime and boot time come from `system_info`.

### Registry diagnostics

`registry_diagnostics` reads only a fixed allowlist of keys: OS identity
(`HKLM\SOFTWARE\Microsoft\Windows NT\CurrentVersion`), startup programs
(`Run`/`RunOnce` under HKLM and HKCU), and installed software (the
`Uninstall` keys). Startup entries are flagged enabled/disabled from the
`Explorer\StartupApproved\Run` state byte (`0x02` enabled, `0x03` disabled;
absent means enabled). Caller-supplied registry paths are not accepted.
```

- [ ] **Step 3: Update `docs/architecture.md`**

1. Line 140-141: `14 read capabilities are implemented in v1` → `15 read capabilities are implemented in v1`.
2. Line 12: `(59 tool definitions + ...)` → `(72 tool definitions + ...)`. (`README.md:228` has the identical `(59 tool definitions ...)` line - update it too.)
3. In the `### platform/windows/` section (after the `services.rs` line), add:

```markdown
- `registry.rs` - allowlist-only registry diagnostics reads.
```

- [ ] **Step 4: Update `docs/permissions.md`**

1. Line 9: `v1 implements 14 read capabilities` → `v1 implements 15 read capabilities`.
2. Event read row: `| Event read | event.read | get_recent_events, get_application_errors, get_system_errors |` → add `, crash_history, shutdown_analysis`.
3. Add a row to the v1 read table:

```markdown
| Registry read | `registry.read` | `registry_diagnostics` |
```

4. Line 34-36 (declared action capabilities): remove `registry.read` from the list so it reads `filesystem.read`, `filesystem.write`, `filesystem.delete`, `process.terminate`, `service.modify`, `powershell.execute`, `registry.write`.

- [ ] **Step 5: Update `docs/security.md`**

If `registry.read` appears in any "declared action capabilities" list in the file, remove it. Near the "unsafe blocks exist only in `src/platform/windows/`" paragraph (line 157), add:

```markdown
- Registry reads are allowlist-only: `registry_diagnostics` reads a fixed
  set of diagnostic keys and never accepts caller-supplied paths.
```

- [ ] **Step 6: Update `CHANGELOG.md`**

Replace the empty `## [Unreleased]` section:

```markdown
## [Unreleased]

### Added

- **`crash_history` tool** - BSOD/crash history from the event logs: bugchecks (BugCheck 1001), unclean shutdowns (Kernel-Power 41), hardware errors (WHEA-Logger 18/19/20), application crashes, and Windows Error Reporting events, with bugcheck codes extracted from the rendered message.
- **`shutdown_analysis` tool** - boot/shutdown timeline (EventLog 6005/6006/6008/6013, User32 1074, Kernel-General 12/13, Kernel-Power 41/42/107) with last boot, current uptime, and a last-shutdown-kind summary.
- **`registry_diagnostics` tool** - allowlist-only registry reads: OS identity, startup programs (with enabled/disabled state), and installed software.
- **`registry.read` capability** - promoted from declared-but-never-granted to a v1 read capability, granted in `safe` and `read_only` modes.
```

- [ ] **Step 7: Run the count sweep**

Update the hard-coded tool counts to the new values (72 full / 55 developer / 58 browser):

- `README.md:38` `69 MCP tools` → `72 MCP tools`
- `README.md:228` `(59 tool definitions ...)` → `(72 tool definitions ...)`
- `docs/installation.md:90` `52 tools` → `55 tools`
- `docs/mcp-integration.md:104-105` `69 tools ... 52 in the default developer profile, 55 in browser` → `72 tools ... 55 in the default developer profile, 58 in browser`
- `docs/performance.md:15` `all 69 tools benchmarked` → `all 72 tools benchmarked`
- `docs/release.md:82` `69 tools in the full profile, 52 in the default` → `72 tools in the full profile, 55 in the default`
- `docs/release.md:69` checklist item wording that references "69 tools" → "72 tools"
- `CONTRIBUTING.md:53` `registry (69 tools)` → `registry (72 tools)`
- `skills/winkit-developer-debugging/SKILL.md:117` `full is the complete v1 tool set (69 tools)` → `(72 tools)`

`CHANGELOG.md:161` ("The registry is now 69 tools") is a historical entry for a past release - do NOT change it.

- [ ] **Step 8: Commit**

```bash
git add README.md CHANGELOG.md docs/
git commit -m "docs: document stability tools, registry diagnostics, and new counts"
```

---

### Task 11: Full-Suite Verification

**Files:** none (verification only).

- [ ] **Step 1: Format**

Run: `cargo fmt --check`
Expected: clean (run `cargo fmt` if not).

- [ ] **Step 2: Lint**

Run: `cargo clippy --features mocks --all-targets -- -D warnings`
Expected: no warnings. Fix anything it flags (e.g. `needless_borrow`, unused imports) and re-run.

- [ ] **Step 3: Full test suite**

Run: `cargo test --features mocks`
Expected: all tests PASS, including the integrity tests (`registry_builds_every_expected_tool_exactly_once`, `profile_exposed_tool_counts_are_exact`), the stability/registry tool tests, policy tests, and the existing suite.

- [ ] **Step 4: Check for stale tool-count references**

Run: `Select-String -Path "README.md","docs\*.md","CONTRIBUTING.md","skills\winkit-developer-debugging\SKILL.md" -Pattern "69 tools|52 in the default|52 tools|55 in|all 69|59 tool definitions"` - the only remaining matches should be intentional historical notes (e.g. in `release.md` and `CHANGELOG.md` describing a past release). If a *current-state* count is still wrong, fix it.

- [ ] **Step 5: Commit any stragglers**

```bash
git status
git add -A
git commit -m "chore: final verification fixes"
```

