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

    /// Check a managed-browser action: feature flag first, then the approval
    /// requirement for the capability. The `feature_enabled` argument is the
    /// `[chrome.managed] enabled` configuration value.
    pub fn check_browser_action(
        &self,
        capability: Capability,
        tool: &str,
        feature_enabled: bool,
    ) -> Result<(), crate::errors::WinkitError> {
        self.approvals
            .check_browser_action(capability, tool, feature_enabled)
    }

    /// Grant a pending approval request by id (approval-mode only).
    pub fn grant_approval(&self, request_id: u64) -> Result<(), crate::errors::WinkitError> {
        self.approvals.grant(request_id)
    }

    /// Pending (not yet granted) approval requests, newest first.
    pub fn pending_approvals(&self) -> Vec<ApprovalRequest> {
        self.approvals.pending_requests()
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
            .check(Capability::ApplicationTabsRead, "chrome_list_tabs")
            .is_err());
    }

    #[test]
    fn browser_actions_follow_mode_and_flag() {
        let safe = PermissionManager::new(PermissionMode::Safe);
        let err = safe
            .check_browser_action(
                Capability::BrowserLaunch,
                "chrome_start_managed_session",
                true,
            )
            .unwrap_err();
        assert_eq!(err.kind, crate::errors::ErrorKind::PermissionDenied);
        assert!(err.message.contains("unrestricted"));

        let ro = PermissionManager::new(PermissionMode::ReadOnly);
        assert!(ro
            .check_browser_action(
                Capability::BrowserLaunch,
                "chrome_start_managed_session",
                true
            )
            .is_err());

        let unrestricted = PermissionManager::new(PermissionMode::Unrestricted);
        assert!(unrestricted
            .check_browser_action(
                Capability::BrowserLaunch,
                "chrome_start_managed_session",
                true
            )
            .is_ok());
        // Feature flag is enforced independently of the mode.
        assert!(unrestricted
            .check_browser_action(
                Capability::BrowserLaunch,
                "chrome_start_managed_session",
                false
            )
            .is_err());
    }

    #[test]
    fn approval_mode_requires_explicit_grant() {
        let approval = PermissionManager::new(PermissionMode::Approval);
        let err = approval
            .check_browser_action(
                Capability::BrowserNavigate,
                "chrome_navigate_managed_session",
                true,
            )
            .unwrap_err();
        assert_eq!(err.kind, crate::errors::ErrorKind::ApprovalRequired);
        let id: u64 = err
            .message
            .split("request_id = ")
            .nth(1)
            .and_then(|s| s.split(')').next())
            .and_then(|s| s.trim().parse().ok())
            .expect("message embeds the request id");
        assert!(approval.grant_approval(id).is_ok());
        // The explicit grant is consumed by the retry of the same action,
        // so the workflow is usable: grant, then retry.
        assert!(approval
            .check_browser_action(
                Capability::BrowserNavigate,
                "chrome_navigate_managed_session",
                true,
            )
            .is_ok());
        // Grants are per-request, never a standing permission: a fresh
        // action still requires a fresh approval.
        let err2 = approval
            .check_browser_action(
                Capability::BrowserNavigate,
                "chrome_navigate_managed_session",
                true,
            )
            .unwrap_err();
        assert_eq!(err2.kind, crate::errors::ErrorKind::ApprovalRequired);
        // Only the fresh request is still pending; the granted one was consumed.
        assert_eq!(approval.pending_approvals().len(), 1);
    }
}
