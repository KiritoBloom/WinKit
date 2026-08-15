//! Capability model: every read capability WinKit can grant, plus the
//! future action capabilities that are declared but never enabled in v1.

use serde::Serialize;
use std::str::FromStr;

/// All capabilities WinKit knows about.
///
/// v1 implements the `*_read` and `application.*` capabilities. The
/// write/action capabilities (`filesystem.write`, `process.terminate`, ...)
/// are declared so that policies and documentation stay stable, but nothing
/// in v1 can ever be granted them — they always fail closed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Capability {
    // Windows-level read capabilities (v1).
    SystemRead,
    ProcessRead,
    NetworkRead,
    StorageRead,
    ServiceRead,
    EventRead,
    WindowRead,

    // Application-level read capabilities (v1).
    ApplicationDiscover,
    ApplicationTabsRead,
    ApplicationPerformanceRead,
    ApplicationMemoryRead,
    ApplicationNetworkRead,
    ApplicationRuntimeRead,
    ApplicationDiagnosticsRead,

    // Managed-browser action capabilities (permission-gated, feature-gated).
    BrowserLaunch,
    BrowserNavigate,
    BrowserClose,

    // Future action capabilities — declared, never granted in v1.
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
            Self::ApplicationDiscover => "application.discover",
            Self::ApplicationTabsRead => "application.tabs.read",
            Self::ApplicationPerformanceRead => "application.performance.read",
            Self::ApplicationMemoryRead => "application.memory.read",
            Self::ApplicationNetworkRead => "application.network.read",
            Self::ApplicationRuntimeRead => "application.runtime.read",
            Self::ApplicationDiagnosticsRead => "application.diagnostics.read",
            Self::BrowserLaunch => "application.browser.launch",
            Self::BrowserNavigate => "application.browser.navigate",
            Self::BrowserClose => "application.browser.close",
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

    /// All capabilities implemented in v1.
    pub const V1_READ_CAPABILITIES: &'static [Capability] = &[
        Capability::SystemRead,
        Capability::ProcessRead,
        Capability::NetworkRead,
        Capability::StorageRead,
        Capability::ServiceRead,
        Capability::EventRead,
        Capability::WindowRead,
        Capability::ApplicationDiscover,
        Capability::ApplicationTabsRead,
        Capability::ApplicationPerformanceRead,
        Capability::ApplicationMemoryRead,
        Capability::ApplicationNetworkRead,
        Capability::ApplicationRuntimeRead,
        Capability::ApplicationDiagnosticsRead,
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
            "application.discover" => Ok(Self::ApplicationDiscover),
            "application.tabs.read" => Ok(Self::ApplicationTabsRead),
            "application.performance.read" => Ok(Self::ApplicationPerformanceRead),
            "application.memory.read" => Ok(Self::ApplicationMemoryRead),
            "application.network.read" => Ok(Self::ApplicationNetworkRead),
            "application.runtime.read" => Ok(Self::ApplicationRuntimeRead),
            "application.diagnostics.read" => Ok(Self::ApplicationDiagnosticsRead),
            "application.browser.launch" => Ok(Self::BrowserLaunch),
            "application.browser.navigate" => Ok(Self::BrowserNavigate),
            "application.browser.close" => Ok(Self::BrowserClose),
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

/// Is this capability one of the v1 read-only capabilities?
pub fn is_v1_read_capability(c: Capability) -> bool {
    Capability::V1_READ_CAPABILITIES.contains(&c)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capability_names_match_protocol() {
        assert_eq!(Capability::SystemRead.as_str(), "system.read");
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
