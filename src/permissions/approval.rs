//! Approval architecture.
//!
//! WinKit is read-only, so no tool ever requires approval. This module
//! defines the API surface that future action capabilities would flow
//! through: a request is created, a policy decides whether approval is
//! required, and — in a future release — an external approver (MCP client,
//! UI, policy file) can grant or deny it.

use crate::errors::WinkitError;
use crate::permissions::capability::Capability;
use serde::Serialize;
use std::sync::atomic::{AtomicU64, Ordering};

static NEXT_REQUEST_ID: AtomicU64 = AtomicU64::new(1);

/// The state of an approval request.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ApprovalStatus {
    Pending,
    Approved,
    Denied,
    Expired,
}

/// A future tool invocation that may require approval.
#[derive(Debug, Clone, Serialize)]
pub struct ApprovalRequest {
    pub request_id: u64,
    pub capability: Capability,
    pub tool: String,
    /// Truncated, non-sensitive description of the intended action.
    pub description: String,
    pub status: ApprovalStatus,
    /// RFC3339 creation time.
    pub created_at: String,
}

impl ApprovalRequest {
    fn new(capability: Capability, tool: &str, description: String) -> Self {
        Self {
            request_id: NEXT_REQUEST_ID.fetch_add(1, Ordering::Relaxed),
            capability,
            tool: tool.to_string(),
            description,
            status: ApprovalStatus::Pending,
            created_at: crate::utils::time::format_rfc3339(std::time::SystemTime::now()),
        }
    }
}

/// How a capability is treated under the current mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApprovalRequirement {
    /// Always allowed by policy.
    Allowed,
    /// Requires an approval before execution.
    Required,
    /// Never allowed in this build.
    Denied,
}

/// The approval manager. Read capabilities resolve to `Allowed` (for
/// granted reads) or `Denied`; every action capability is denied because
/// WinKit is read-only.
#[derive(Debug, Clone)]
pub struct ApprovalManager {
    policy: crate::permissions::Policy,
}

impl ApprovalManager {
    pub fn new(policy: crate::permissions::Policy) -> Self {
        Self { policy }
    }

    /// Decide what is needed to run a capability.
    ///
    /// Read capabilities follow the policy grant set; every action
    /// capability is denied (WinKit is read-only).
    pub fn requirement_for(&self, capability: Capability) -> ApprovalRequirement {
        if crate::permissions::capability::is_v1_read_capability(capability) {
            return if self.policy.allows(capability) {
                ApprovalRequirement::Allowed
            } else {
                ApprovalRequirement::Denied
            };
        }
        ApprovalRequirement::Denied
    }

    /// Create an approval request. No current caller reaches this path;
    /// it exists for a future action layer.
    pub fn request(
        &self,
        capability: Capability,
        tool: &str,
        description: String,
    ) -> Result<ApprovalRequest, WinkitError> {
        match self.requirement_for(capability) {
            ApprovalRequirement::Allowed => Err(WinkitError::internal(
                "approval requested for a capability that is already allowed",
            )),
            ApprovalRequirement::Denied => {
                Err(WinkitError::permission_denied(capability.as_str(), tool))
            }
            ApprovalRequirement::Required => Ok(ApprovalRequest::new(
                capability,
                tool,
                crate::utils::truncate(&description, 500),
            )),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::permissions::Policy;

    #[test]
    fn read_capabilities_are_allowed_and_actions_denied() {
        let manager = ApprovalManager::new(Policy::for_mode(
            crate::permissions::PermissionMode::ReadOnly,
        ));
        assert_eq!(
            manager.requirement_for(Capability::SystemRead),
            ApprovalRequirement::Allowed
        );
        assert_eq!(
            manager.requirement_for(Capability::ProcessTerminate),
            ApprovalRequirement::Denied
        );
    }

    #[test]
    fn denied_capabilities_produce_permission_errors() {
        let manager = ApprovalManager::new(Policy::for_mode(
            crate::permissions::PermissionMode::ReadOnly,
        ));
        let err = manager
            .request(Capability::ProcessTerminate, "kill_process", "kill".into())
            .unwrap_err();
        assert_eq!(err.kind, crate::errors::ErrorKind::PermissionDenied);
    }
}
