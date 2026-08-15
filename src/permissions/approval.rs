//! Approval architecture.
//!
//! v1 is read-only, so no tool ever requires approval. This module defines
//! the API surface that future write/action capabilities will flow through:
//! a request is created, a policy decides whether approval is required, and
//! — in a future release — an external approver (MCP client, UI, policy
//! file) can grant or deny it.

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
/// granted reads) or `Denied`; managed-browser action capabilities resolve
/// by permission mode. `Required` is also the path future write capabilities
/// will take.
#[derive(Debug, Clone)]
pub struct ApprovalManager {
    policy: crate::permissions::Policy,
    /// Approval requests awaiting an explicit grant (`request_id` order).
    pending: std::sync::Arc<std::sync::Mutex<Vec<ApprovalRequest>>>,
    /// Explicitly granted request ids.
    approved: std::sync::Arc<std::sync::Mutex<std::collections::HashSet<u64>>>,
}

impl ApprovalManager {
    pub fn new(policy: crate::permissions::Policy) -> Self {
        Self {
            policy,
            pending: std::sync::Arc::new(std::sync::Mutex::new(Vec::new())),
            approved: std::sync::Arc::new(std::sync::Mutex::new(std::collections::HashSet::new())),
        }
    }

    /// Decide what is needed to run a capability.
    ///
    /// Read capabilities follow the policy grant set. Managed-browser action
    /// capabilities follow the mode: `safe`/`read_only` deny them, `approval`
    /// requires an explicit grant, `unrestricted` allows them (the tool layer
    /// still enforces the feature flag and every validation rule).
    pub fn requirement_for(&self, capability: Capability) -> ApprovalRequirement {
        if crate::permissions::capability::is_v1_read_capability(capability) {
            return if self.policy.allows(capability) {
                ApprovalRequirement::Allowed
            } else {
                ApprovalRequirement::Denied
            };
        }
        match capability {
            Capability::BrowserLaunch | Capability::BrowserNavigate | Capability::BrowserClose => {
                match self.policy.mode {
                    crate::permissions::PermissionMode::Safe
                    | crate::permissions::PermissionMode::ReadOnly => ApprovalRequirement::Denied,
                    crate::permissions::PermissionMode::Approval => ApprovalRequirement::Required,
                    crate::permissions::PermissionMode::Unrestricted => {
                        ApprovalRequirement::Allowed
                    }
                }
            }
            // Other unimplemented action capabilities: never allowed.
            _ => ApprovalRequirement::Denied,
        }
    }

    /// Check a browser action against the approval state.
    ///
    /// * Feature disabled — `FeatureDisabled` error.
    /// * `Allowed` — proceed.
    /// * `Denied` — permission error explaining what would be required.
    /// * `Required` — create a pending request and return it; the action is
    ///   blocked until [`Self::grant`] is called with its id.
    pub fn check_browser_action(
        &self,
        capability: Capability,
        tool: &str,
        feature_enabled: bool,
    ) -> Result<(), WinkitError> {
        if !feature_enabled {
            return Err(WinkitError::new(
                crate::errors::ErrorKind::FeatureDisabled,
                format!(
                    "tool '{tool}' requires the managed-browser feature: set [chrome.managed] enabled = true in configuration"
                ),
            ));
        }
        match self.requirement_for(capability) {
            ApprovalRequirement::Allowed => Ok(()),
            ApprovalRequirement::Denied => Err(WinkitError::permission_denied_browser(
                capability.as_str(),
                tool,
                self.policy.mode.as_str(),
            )),
            ApprovalRequirement::Required => {
                // A prior explicit grant for this capability+tool is consumed
                // by the retry, so approval mode is usable: the agent grants
                // the returned request_id, then retries the same action.
                if self.consume_approved(capability, tool) {
                    return Ok(());
                }
                let request =
                    self.create_request(capability, tool, "managed browser action".into())?;
                Err(WinkitError::approval_required(
                    request.request_id,
                    capability.as_str(),
                    tool,
                ))
            }
        }
    }

    /// Consume one explicitly granted pending request for `capability`+`tool`.
    /// Grants are per-request, never a standing permission.
    fn consume_approved(&self, capability: Capability, tool: &str) -> bool {
        let mut pending = match self.pending.lock() {
            Ok(g) => g,
            Err(_) => return false,
        };
        let idx = pending.iter().position(|r| {
            r.capability == capability && r.tool == tool && self.is_approved(r.request_id)
        });
        match idx {
            Some(i) => {
                let id = pending.remove(i).request_id;
                if let Ok(mut approved) = self.approved.lock() {
                    approved.remove(&id);
                }
                true
            }
            None => false,
        }
    }

    fn create_request(
        &self,
        capability: Capability,
        tool: &str,
        description: String,
    ) -> Result<ApprovalRequest, WinkitError> {
        let request =
            ApprovalRequest::new(capability, tool, crate::utils::truncate(&description, 500));
        self.pending
            .lock()
            .map_err(|_| WinkitError::internal("approval queue poisoned"))?
            .push(request.clone());
        Ok(request)
    }

    /// Grant a pending request by id. Returns an error for unknown or already
    /// granted ids.
    pub fn grant(&self, request_id: u64) -> Result<(), WinkitError> {
        let pending = self
            .pending
            .lock()
            .map_err(|_| WinkitError::internal("approval queue poisoned"))?;
        let known = pending.iter().any(|r| r.request_id == request_id);
        if !known {
            return Err(WinkitError::new(
                crate::errors::ErrorKind::NotFound,
                format!("no pending approval request with id {request_id}"),
            ));
        }
        self.approved
            .lock()
            .map_err(|_| WinkitError::internal("approval queue poisoned"))?
            .insert(request_id);
        Ok(())
    }

    /// Has this request been explicitly granted?
    pub fn is_approved(&self, request_id: u64) -> bool {
        self.approved
            .lock()
            .map(|g| g.contains(&request_id))
            .unwrap_or(false)
    }

    /// All pending (not yet granted) approval requests, newest first.
    pub fn pending_requests(&self) -> Vec<ApprovalRequest> {
        let mut all = self.pending.lock().map(|p| p.clone()).unwrap_or_default();
        all.retain(|r| !self.is_approved(r.request_id));
        all.reverse();
        all
    }

    /// Create an approval request. v1 callers should never reach this path;
    /// it exists for the future action layer.
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
    fn v1_capabilities_are_never_approval_required() {
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
