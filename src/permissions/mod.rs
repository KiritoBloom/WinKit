//! Permission subsystem: capabilities, policies, and approval.

pub mod approval;
pub mod capability;
pub mod policy;

pub use approval::{ApprovalManager, ApprovalRequest, ApprovalRequirement, ApprovalStatus};
pub use capability::{is_v1_read_capability, Capability};
pub use policy::{PermissionMode, Policy};

/// The permission manager used by tools: policy lookup + error mapping.
#[derive(Debug, Clone)]
pub struct PermissionManager {
    pub mode: PermissionMode,
    policy: Policy,
    approvals: ApprovalManager,
}

impl PermissionManager {
    pub fn new(mode: PermissionMode) -> Self {
        let policy = Policy::for_mode(mode);
        let approvals = ApprovalManager::new(policy.clone());
        Self {
            mode,
            policy,
            approvals,
        }
    }

    /// Enforce that `capability` is granted; returns a `PermissionDenied`
    /// error naming the tool otherwise.
    pub fn check(
        &self,
        capability: Capability,
        tool: &str,
    ) -> Result<(), crate::errors::WinkitError> {
        match self.approvals.requirement_for(capability) {
            ApprovalRequirement::Allowed => Ok(()),
            _ => Err(crate::errors::WinkitError::permission_denied(
                capability.as_str(),
                tool,
            )),
        }
    }

    pub fn policy(&self) -> &Policy {
        &self.policy
    }

    pub fn approvals(&self) -> &ApprovalManager {
        &self.approvals
    }

    /// Human-readable summary for `system_info`/logging.
    pub fn describe(&self) -> serde_json::Value {
        let granted: Vec<String> = self
            .policy()
            .granted_capabilities()
            .iter()
            .map(|c| c.as_str().to_string())
            .collect();
        serde_json::json!({
            "mode": self.mode.as_str(),
            "granted_capabilities": granted,
            "read_only": true,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn check_enforces_mode() {
        let safe = PermissionManager::new(PermissionMode::Safe);
        assert!(safe.check(Capability::SystemRead, "system_info").is_ok());
        assert!(safe
            .check(Capability::ProcessTerminate, "kill_process")
            .is_err());
    }
}
