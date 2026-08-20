//! Permission mode and policy evaluation.
//!
//! v1 supports four modes. Only `safe` and `read_only` are meaningful for
//! the capabilities that exist in v1; `approval` and `unrestricted` are
//! declared for forward compatibility and never enable capabilities that
//! are not implemented.

use crate::permissions::capability::{is_v1_read_capability, Capability};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

/// Permission modes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PermissionMode {
    /// Conservative: Windows-level read capabilities only. Application
    /// adapters are discoverable but their deep inspection is off.
    Safe,
    /// All v1 read capabilities (Windows + applications).
    ReadOnly,
    /// Architecturally reserved for future write/action capabilities that
    /// require interactive approval. In v1 this behaves like `read_only`
    /// because no action capabilities exist.
    Approval,
    /// Architecturally reserved. In v1 this enables exactly the read
    /// capabilities that exist — it cannot enable anything else.
    Unrestricted,
}

impl PermissionMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Safe => "safe",
            Self::ReadOnly => "read_only",
            Self::Approval => "approval",
            Self::Unrestricted => "unrestricted",
        }
    }

    /// Parse a mode name, tolerating dashes.
    pub fn parse(s: &str) -> Option<Self> {
        match s.to_ascii_lowercase().replace('-', "_").as_str() {
            "safe" => Some(Self::Safe),
            "read_only" | "readonly" => Some(Self::ReadOnly),
            "approval" => Some(Self::Approval),
            "unrestricted" => Some(Self::Unrestricted),
            _ => None,
        }
    }
}

/// The permission policy derived from a mode.
#[derive(Debug, Clone)]
pub struct Policy {
    pub mode: PermissionMode,
    /// Capabilities explicitly granted by the policy.
    granted: BTreeSet<Capability>,
}

impl Policy {
    /// Build the v1 policy for a mode. Action capabilities are never granted
    /// because none are implemented in v1.
    pub fn for_mode(mode: PermissionMode) -> Self {
        let mut granted = BTreeSet::new();
        match mode {
            PermissionMode::Safe => {
                for c in Capability::V1_READ_CAPABILITIES {
                    if matches!(
                        c,
                        Capability::SystemRead
                            | Capability::ProcessRead
                            | Capability::NetworkRead
                            | Capability::StorageRead
                            | Capability::ServiceRead
                            | Capability::EventRead
                            | Capability::WindowRead
                            | Capability::RegistryRead
                    ) {
                        granted.insert(*c);
                    }
                }
            }
            PermissionMode::ReadOnly | PermissionMode::Approval | PermissionMode::Unrestricted => {
                for c in Capability::V1_READ_CAPABILITIES {
                    granted.insert(*c);
                }
            }
        }
        Self { mode, granted }
    }

    pub fn allows(&self, capability: Capability) -> bool {
        // Fail closed: capabilities that are not v1 read capabilities are
        // never allowed, regardless of mode.
        is_v1_read_capability(capability) && self.granted.contains(&capability)
    }

    pub fn granted_capabilities(&self) -> Vec<Capability> {
        self.granted.iter().copied().collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn safe_mode_allows_windows_reads_only() {
        let p = Policy::for_mode(PermissionMode::Safe);
        assert!(p.allows(Capability::SystemRead));
        assert!(p.allows(Capability::WindowRead));
        assert!(!p.allows(Capability::ApplicationTabsRead));
        assert!(!p.allows(Capability::ProcessTerminate));
    }

    #[test]
    fn read_only_mode_allows_all_v1_reads() {
        let p = Policy::for_mode(PermissionMode::ReadOnly);
        assert!(p.allows(Capability::SystemRead));
        assert!(p.allows(Capability::ApplicationDiagnosticsRead));
    }

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

    #[test]
    fn approval_and_unrestricted_never_enable_unimplemented_capabilities() {
        for mode in [PermissionMode::Approval, PermissionMode::Unrestricted] {
            let p = Policy::for_mode(mode);
            assert!(!p.allows(Capability::ProcessTerminate));
            assert!(!p.allows(Capability::PowershellExecute));
            assert!(!p.allows(Capability::FilesystemWrite));
        }
    }

    #[test]
    fn mode_parsing_is_lenient() {
        assert_eq!(
            PermissionMode::parse("read-only"),
            Some(PermissionMode::ReadOnly)
        );
        assert_eq!(
            PermissionMode::parse("READ_ONLY"),
            Some(PermissionMode::ReadOnly)
        );
        assert_eq!(PermissionMode::parse("bogus"), None);
    }
}
