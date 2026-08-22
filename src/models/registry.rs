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

/// One autostart entry (Run/RunOnce keys, Startup folders, Winlogon,
/// boot execute, or Active Setup).
///
/// `impact` is a heuristic estimate computed by
/// [`assess_startup_impact`] — WinKit's read-only data does not include
/// boot performance traces, so exact boot-phase timing is deliberately not
/// claimed.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct StartupProgram {
    pub name: String,
    pub command: String,
    /// `machine` (HKLM / common) or `user` (HKCU / per-user).
    pub scope: String,
    /// Full registry path of the source key, or the Startup folder path.
    pub source_key: String,
    pub enabled: bool,
    /// Where the entry comes from: `run`, `run_once`, `startup_folder`,
    /// `winlogon`, `boot_execute`, or `active_setup`.
    pub entry_type: String,
    /// True when Windows does not list this entry in Task Manager's
    /// Startup apps tab (`run_once`, winlogon, boot execute, Active Setup).
    pub hidden: bool,
    /// Heuristic impact estimate: `high`, `medium`, `low`; `none` when the
    /// entry is disabled and therefore does not affect startup.
    pub impact: String,
    /// Transparent reasons backing the `impact` level.
    pub impact_reasons: Vec<String>,
}

/// Heuristic startup-impact assessment for one entry.
///
/// This is an *estimate* from read-only signals, not a measurement:
///
/// - Disabled entries report `none` — they cannot affect startup until
///   re-enabled.
/// - `winlogon` / `boot_execute` entries run before the logon/desktop
///   phases most users associate with "startup", so they rank `high`.
/// - `active_setup` stubs run once per user at logon: `medium`.
/// - `run_once` entries delete themselves after one run: `low`.
/// - A resolvable executable adds a size signal (≥ 100 MiB → `high`,
///   ≥ 20 MiB → at least `medium`) and executables living under a Temp
///   directory rank `high` regardless of size.
/// - Everything else with no stronger signal is `low`.
///
/// Returns `(level, reasons)` with reasons ordered by strength.
pub fn assess_startup_impact(
    entry_type: &str,
    command: &str,
    exe_size_bytes: Option<u64>,
    enabled: bool,
) -> (String, Vec<String>) {
    const MIB: u64 = 1_048_576;
    if !enabled {
        return (
            "none".to_string(),
            vec!["entry is disabled; it does not run at startup until re-enabled".to_string()],
        );
    }
    // (rank, level, reason) signals; the max wins, ties keep both reasons.
    let mut signals: Vec<(u8, &'static str, String)> = Vec::new();
    match entry_type {
        "winlogon" | "boot_execute" => signals.push((
            3,
            "high",
            format!(
                "{entry_type} entries run during the system boot/logon phase, before Startup-apps entries"
            ),
        )),
        "active_setup" => signals.push((
            2,
            "medium",
            "Active Setup stubs run once per user at logon".to_string(),
        )),
        "run_once" => signals.push((
            1,
            "low",
            "one-shot entry; Windows deletes it after the next logon".to_string(),
        )),
        _ => signals.push((1, "low", "no special-phase signal".to_string())),
    }
    if let Some(size) = exe_size_bytes {
        if size >= 100 * MIB {
            signals.push((
                3,
                "high",
                format!("executable is large (~{} MB)", size / MIB),
            ));
        } else if size >= 20 * MIB {
            signals.push((
                2,
                "medium",
                format!("executable is sizable (~{} MB)", size / MIB),
            ));
        }
    }
    if let Some(path) = extract_executable_path(command) {
        let lowered = path.to_ascii_lowercase();
        if lowered.contains("\\temp\\") || lowered.ends_with("\\temp") {
            signals.push((
                3,
                "high",
                "executable lives in a temporary directory".to_string(),
            ));
        }
    }
    signals.sort_by_key(|(rank, _, _)| std::cmp::Reverse(*rank));
    let top_rank = signals[0].0;
    let level = signals[0].1;
    let reasons: Vec<String> = signals
        .iter()
        .filter(|(rank, _, _)| *rank == top_rank)
        .map(|(_, _, reason)| reason.clone())
        .collect();
    (level.to_string(), reasons)
}

/// Extract the first executable-looking token from a command line:
/// quote-aware, `reg.exe`-style arguments ignored. Returns `None` when the
/// first token does not end in `.exe` (scripts, flags-only commands).
pub fn extract_executable_path(command: &str) -> Option<&str> {
    let trimmed = command.trim();
    let first_token = if let Some(rest) = trimmed.strip_prefix('"') {
        rest.split('"').next()?
    } else {
        trimmed.split_whitespace().next()?
    };
    // Windows paths are case-insensitive; accept any casing of .exe while
    // preserving the original text.
    if first_token.len() >= 4 && first_token.to_ascii_lowercase().ends_with(".exe") {
        Some(first_token)
    } else {
        None
    }
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
                entry_type: "run".into(),
                hidden: false,
                impact: "low".into(),
                impact_reasons: vec!["no special-phase signal".into()],
            }],
            installed_software: vec![InstalledSoftware {
                name: "Git".into(),
                version: Some("2.45.0".into()),
                publisher: Some("The Git Development Community".into()),
                install_date: Some("20240601".into()),
            }],
            counts: RegistryCounts {
                startup_programs: 1,
                installed_software: 1,
            },
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

    #[test]
    fn disabled_entries_have_none_impact() {
        let (level, reasons) =
            assess_startup_impact("run", "C:\\big.exe", Some(500_000_000), false);
        assert_eq!(level, "none");
        assert!(reasons[0].contains("disabled"));
    }

    #[test]
    fn boot_phase_and_winlogon_entries_rank_high() {
        for kind in ["winlogon", "boot_execute"] {
            let (level, _) =
                assess_startup_impact(kind, "C:\\Windows\\System32\\autochk.exe", None, true);
            assert_eq!(level, "high", "{kind} must rank high");
        }
    }

    #[test]
    fn run_once_defaults_to_low_impact() {
        let (level, reasons) =
            assess_startup_impact("run_once", "C:\\Tools\\setup.exe /finalize", None, true);
        assert_eq!(level, "low");
        assert!(reasons[0].contains("one-shot"));
    }

    #[test]
    fn executable_size_raises_impact() {
        // ~120 MB → high even for a plain Run entry.
        let (level, reasons) =
            assess_startup_impact("run", "C:\\Apps\\heavy.exe", Some(120 * 1_048_576), true);
        assert_eq!(level, "high");
        assert!(reasons.iter().any(|r| r.contains("large")));
        // ~30 MB → medium.
        let (level, reasons) =
            assess_startup_impact("run", "C:\\Apps\\mid.exe", Some(30 * 1_048_576), true);
        assert_eq!(level, "medium");
        assert!(reasons.iter().any(|r| r.contains("sizable")));
    }

    #[test]
    fn temp_directory_executables_rank_high() {
        let (level, _) = assess_startup_impact(
            "run",
            "\"C:\\Users\\u\\AppData\\Local\\Temp\\installer.exe\" /q",
            None,
            true,
        );
        assert_eq!(level, "high");
    }

    #[test]
    fn strongest_signal_wins_with_all_reasons_at_that_level() {
        let (level, reasons) = assess_startup_impact(
            "run_once",
            "C:\\Users\\u\\AppData\\Local\\Temp\\stub.exe",
            Some(200 * 1_048_576),
            true,
        );
        assert_eq!(level, "high");
        assert_eq!(reasons.len(), 2, "both high signals reported");
    }

    #[test]
    fn extract_executable_path_handles_quotes_and_args() {
        assert_eq!(
            extract_executable_path("\"C:\\Program Files\\X\\app.exe\" --minimized"),
            Some("C:\\Program Files\\X\\app.exe")
        );
        assert_eq!(
            extract_executable_path("C:\\Tools\\old.exe"),
            Some("C:\\Tools\\old.exe")
        );
        assert_eq!(
            extract_executable_path("rundll32.exe shell32.dll,x"),
            Some("rundll32.exe")
        );
        // Non-exe first tokens and empty commands yield None.
        assert_eq!(extract_executable_path("cmd /c echo hi"), None);
        assert_eq!(extract_executable_path(""), None);
        assert_eq!(
            extract_executable_path("\"C:\\no closing quote\\x.EXE\""),
            Some("C:\\no closing quote\\x.EXE")
        );
    }
}
