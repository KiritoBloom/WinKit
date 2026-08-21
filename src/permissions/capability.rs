//! Capability model: every read capability WinKit can grant, plus the
//! future action capabilities that are declared but never enabled.

use serde::Serialize;
use std::str::FromStr;

/// All capabilities WinKit knows about.
///
/// WinKit implements the `*_read` capabilities. The write/action
/// capabilities (`filesystem.write`, `process.terminate`, ...) are declared
/// so that policies and documentation stay stable, but nothing can ever be
/// granted them — they always fail closed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Capability {
    // Windows-level read capabilities.
    SystemRead,
    ProcessRead,
    NetworkRead,
    StorageRead,
    ServiceRead,
    EventRead,
    WindowRead,

    // Hardware-level read capabilities.
    HardwareRead,
    StorageHealthRead,
    PowerRead,
    WifiRead,
    NetworkDiagnosticsRead,

    // Future action capabilities — declared, never granted.
    FilesystemRead,
    FilesystemWrite,
    FilesystemDelete,
    ProcessTerminate,
    ServiceModify,
    PowershellExecute,
    RegistryRead,
    RegistryWrite,
}

impl Capability {
    /// Protocol-level name, e.g. `system.read`.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::SystemRead => "system.read",
            Self::ProcessRead => "process.read",
            Self::NetworkRead => "network.read",
            Self::StorageRead => "storage.read",
            Self::ServiceRead => "service.read",
            Self::EventRead => "event.read",
            Self::WindowRead => "window.read",
            Self::HardwareRead => "hardware.read",
            Self::StorageHealthRead => "storage.health.read",
            Self::PowerRead => "hardware.power.read",
            Self::WifiRead => "network.wifi.read",
            Self::NetworkDiagnosticsRead => "network.diagnostics.read",
            Self::FilesystemRead => "filesystem.read",
            Self::FilesystemWrite => "filesystem.write",
            Self::FilesystemDelete => "filesystem.delete",
            Self::ProcessTerminate => "process.terminate",
            Self::ServiceModify => "service.modify",
            Self::PowershellExecute => "powershell.execute",
            Self::RegistryRead => "registry.read",
            Self::RegistryWrite => "registry.write",
        }
    }

    /// All read capabilities WinKit grants.
    pub const V1_READ_CAPABILITIES: &'static [Capability] = &[
        Capability::SystemRead,
        Capability::ProcessRead,
        Capability::NetworkRead,
        Capability::StorageRead,
        Capability::ServiceRead,
        Capability::EventRead,
        Capability::WindowRead,
        Capability::HardwareRead,
        Capability::StorageHealthRead,
        Capability::PowerRead,
        Capability::WifiRead,
        Capability::NetworkDiagnosticsRead,
        Capability::RegistryRead,
    ];
}

impl FromStr for Capability {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "system.read" => Ok(Self::SystemRead),
            "process.read" => Ok(Self::ProcessRead),
            "network.read" => Ok(Self::NetworkRead),
            "storage.read" => Ok(Self::StorageRead),
            "service.read" => Ok(Self::ServiceRead),
            "event.read" => Ok(Self::EventRead),
            "window.read" => Ok(Self::WindowRead),
            "hardware.read" => Ok(Self::HardwareRead),
            "storage.health.read" => Ok(Self::StorageHealthRead),
            "hardware.power.read" => Ok(Self::PowerRead),
            "network.wifi.read" => Ok(Self::WifiRead),
            "network.diagnostics.read" => Ok(Self::NetworkDiagnosticsRead),
            "filesystem.read" => Ok(Self::FilesystemRead),
            "filesystem.write" => Ok(Self::FilesystemWrite),
            "filesystem.delete" => Ok(Self::FilesystemDelete),
            "process.terminate" => Ok(Self::ProcessTerminate),
            "service.modify" => Ok(Self::ServiceModify),
            "powershell.execute" => Ok(Self::PowershellExecute),
            "registry.read" => Ok(Self::RegistryRead),
            "registry.write" => Ok(Self::RegistryWrite),
            other => Err(format!("unknown capability '{other}'")),
        }
    }
}

/// Is this capability one of the read-only capabilities?
pub fn is_v1_read_capability(c: Capability) -> bool {
    Capability::V1_READ_CAPABILITIES.contains(&c)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capability_names_match_protocol() {
        assert_eq!(Capability::SystemRead.as_str(), "system.read");
        assert_eq!(Capability::HardwareRead.as_str(), "hardware.read");
        assert_eq!(Capability::ProcessTerminate.as_str(), "process.terminate");
        assert_eq!(
            "network.read".parse::<Capability>().unwrap(),
            Capability::NetworkRead
        );
        assert!("nonsense".parse::<Capability>().is_err());
    }

    #[test]
    fn action_capabilities_are_not_v1_read_capabilities() {
        assert!(is_v1_read_capability(Capability::SystemRead));
        assert!(!is_v1_read_capability(Capability::ProcessTerminate));
        assert!(!is_v1_read_capability(Capability::FilesystemWrite));
    }
}
