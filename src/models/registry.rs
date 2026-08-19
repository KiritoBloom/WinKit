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
}
