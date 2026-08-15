//! Typed error system for WinKit.
//!
//! Every failure in the system is represented as a [`WinkitError`] carrying a
//! machine-readable [`ErrorKind`] and a human-readable message. Errors are
//! deliberately opaque to MCP clients: raw OS error codes and stack traces are
//! never serialized. The optional `source` is retained internally for local
//! log output only.

use serde::Serialize;

/// Machine-readable error classification, serialized into MCP responses.
/// The numeric [`ErrorKind::code`] is the stable, documented code an agent
/// can match on; the JSON-RPC layer maps it to the spec error codes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorKind {
    /// One or more tool arguments were missing, malformed, or out of range.
    InvalidArgument,
    /// The configured permission mode or capability policy denied the request.
    PermissionDenied,
    /// A provider (Windows, application, ...) is not available.
    ProviderUnavailable,
    /// An application provider exists but the application cannot be reached.
    ApplicationUnavailable,
    /// The provider does not implement the requested capability.
    UnsupportedCapability,
    /// A Windows API call failed.
    WindowsApiError,
    /// A protocol-level failure (MCP, CDP, JSON-RPC).
    ProtocolError,
    /// An operation exceeded its configured time budget.
    Timeout,
    /// An operation hit a configured resource limit.
    ResourceLimit,
    /// WinKit cannot run on this platform.
    UnsupportedPlatform,
    /// The action requires an explicit approval grant first.
    ApprovalRequired,
    /// The feature the tool relies on is disabled in configuration.
    FeatureDisabled,
    /// The requested path is outside the configured allow set or inside a
    /// deny root.
    PathRejected,
    /// The requested URL failed validation (scheme, host, or redirects).
    UrlRejected,
    /// The requested subject does not exist (process, service, session...).
    NotFound,
    /// The application inspection endpoint is unavailable.
    EndpointUnavailable,
    /// The managed browser process exited unexpectedly.
    BrowserExited,
    /// The serialized payload exceeded its configured cap.
    PayloadLimit,
    /// Too many concurrent operations are already running.
    ConcurrencyLimit,
    /// The result is honest but incomplete; providers returned partial data.
    PartialEvidence,
    /// A Windows/Chrome resource WinKit owns failed to clean up.
    CleanupFailure,
    /// An unexpected internal failure.
    InternalError,
}

impl ErrorKind {
    /// Stable, documented error code. Do not reorder: agents may match on
    /// these values.
    pub const fn code(self) -> i32 {
        match self {
            Self::InvalidArgument => 1,
            Self::PermissionDenied => 2,
            Self::ProviderUnavailable => 3,
            Self::ApplicationUnavailable => 4,
            Self::UnsupportedCapability => 5,
            Self::WindowsApiError => 6,
            Self::ProtocolError => 7,
            Self::Timeout => 8,
            Self::ResourceLimit => 9,
            Self::InternalError => 10,
            Self::UnsupportedPlatform => 11,
            Self::ApprovalRequired => 12,
            Self::FeatureDisabled => 13,
            Self::PathRejected => 14,
            Self::UrlRejected => 15,
            Self::NotFound => 16,
            Self::EndpointUnavailable => 17,
            Self::BrowserExited => 18,
            Self::PayloadLimit => 19,
            Self::ConcurrencyLimit => 20,
            Self::PartialEvidence => 21,
            Self::CleanupFailure => 22,
        }
    }
}

impl std::fmt::Display for ErrorKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::InvalidArgument => "invalid_argument",
            Self::PermissionDenied => "permission_denied",
            Self::ProviderUnavailable => "provider_unavailable",
            Self::ApplicationUnavailable => "application_unavailable",
            Self::UnsupportedCapability => "unsupported_capability",
            Self::WindowsApiError => "windows_api_error",
            Self::ProtocolError => "protocol_error",
            Self::Timeout => "timeout",
            Self::ResourceLimit => "resource_limit",
            Self::InternalError => "internal_error",
            Self::UnsupportedPlatform => "unsupported_platform",
            Self::ApprovalRequired => "approval_required",
            Self::FeatureDisabled => "feature_disabled",
            Self::PathRejected => "path_rejected",
            Self::UrlRejected => "url_rejected",
            Self::NotFound => "not_found",
            Self::EndpointUnavailable => "endpoint_unavailable",
            Self::BrowserExited => "browser_exited",
            Self::PayloadLimit => "payload_limit",
            Self::ConcurrencyLimit => "concurrency_limit",
            Self::PartialEvidence => "partial_evidence",
            Self::CleanupFailure => "cleanup_failure",
        })
    }
}

/// The primary error type used across WinKit.
#[derive(Debug, thiserror::Error)]
#[error("{kind}: {message}")]
pub struct WinkitError {
    pub kind: ErrorKind,
    pub message: String,
    #[source]
    pub source: Option<anyhow::Error>,
}

impl WinkitError {
    pub fn new(kind: ErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
            source: None,
        }
    }

    pub fn with_source(mut self, source: impl std::error::Error + Send + Sync + 'static) -> Self {
        self.source = Some(anyhow::Error::new(source));
        self
    }

    pub fn invalid_argument(message: impl Into<String>) -> Self {
        Self::new(ErrorKind::InvalidArgument, message)
    }

    pub fn permission_denied(capability: &str, tool: &str) -> Self {
        Self::new(
            ErrorKind::PermissionDenied,
            format!("capability '{capability}' is required to use tool '{tool}'"),
        )
    }

    /// Denial that explains the capability, permission mode, feature flag,
    /// and configuration a caller would need to change.
    pub fn permission_denied_browser(capability: &str, tool: &str, mode: &str) -> Self {
        Self::new(
            ErrorKind::PermissionDenied,
            format!(
                "tool '{tool}' requires capability '{capability}', which permission mode '{mode}' does not grant. \
                 Managed-browser actions need permission mode 'approval' (with an explicit grant) or 'unrestricted', \
                 and the [chrome.managed] enabled = true feature flag in configuration."
            ),
        )
    }

    /// The action requires an explicit approval grant before retrying.
    pub fn approval_required(request_id: u64, capability: &str, tool: &str) -> Self {
        Self::new(
            ErrorKind::ApprovalRequired,
            format!(
                "tool '{tool}' needs approval for capability '{capability}'. \
                 Grant it with chrome_approve_managed_action(request_id = {request_id}), then retry."
            ),
        )
    }

    pub fn unsupported_platform(message: impl Into<String>) -> Self {
        Self::new(ErrorKind::UnsupportedPlatform, message)
    }

    pub fn feature_disabled(message: impl Into<String>) -> Self {
        Self::new(ErrorKind::FeatureDisabled, message)
    }

    pub fn path_rejected(message: impl Into<String>) -> Self {
        Self::new(ErrorKind::PathRejected, message)
    }

    pub fn url_rejected(message: impl Into<String>) -> Self {
        Self::new(ErrorKind::UrlRejected, message)
    }

    pub fn not_found(message: impl Into<String>) -> Self {
        Self::new(ErrorKind::NotFound, message)
    }

    pub fn endpoint_unavailable(message: impl Into<String>) -> Self {
        Self::new(ErrorKind::EndpointUnavailable, message)
    }

    pub fn browser_exited(message: impl Into<String>) -> Self {
        Self::new(ErrorKind::BrowserExited, message)
    }

    pub fn payload_limit(message: impl Into<String>) -> Self {
        Self::new(ErrorKind::PayloadLimit, message)
    }

    pub fn concurrency_limit(message: impl Into<String>) -> Self {
        Self::new(ErrorKind::ConcurrencyLimit, message)
    }

    pub fn partial_evidence(message: impl Into<String>) -> Self {
        Self::new(ErrorKind::PartialEvidence, message)
    }

    pub fn cleanup_failure(message: impl Into<String>) -> Self {
        Self::new(ErrorKind::CleanupFailure, message)
    }

    pub fn provider_unavailable(message: impl Into<String>) -> Self {
        Self::new(ErrorKind::ProviderUnavailable, message)
    }

    pub fn application_unavailable(message: impl Into<String>) -> Self {
        Self::new(ErrorKind::ApplicationUnavailable, message)
    }

    pub fn unsupported_capability(message: impl Into<String>) -> Self {
        Self::new(ErrorKind::UnsupportedCapability, message)
    }

    pub fn windows_api(api: &str) -> Self {
        Self::new(
            ErrorKind::WindowsApiError,
            format!("Windows API call '{api}' failed"),
        )
    }

    pub fn protocol(message: impl Into<String>) -> Self {
        Self::new(ErrorKind::ProtocolError, message)
    }

    pub fn timeout(message: impl Into<String>) -> Self {
        Self::new(ErrorKind::Timeout, message)
    }

    pub fn resource_limit(message: impl Into<String>) -> Self {
        Self::new(ErrorKind::ResourceLimit, message)
    }

    pub fn internal(message: impl Into<String>) -> Self {
        Self::new(ErrorKind::InternalError, message)
    }

    /// Serialize this error into the compact shape used in MCP tool results.
    pub fn to_payload(&self) -> serde_json::Value {
        serde_json::json!({
            "error": {
                "code": self.kind.code(),
                "kind": self.kind,
                "message": self.message,
            }
        })
    }
}

impl From<serde_json::Error> for WinkitError {
    fn from(e: serde_json::Error) -> Self {
        Self::new(ErrorKind::InternalError, format!("JSON error: {e}")).with_source(e)
    }
}

impl From<std::io::Error> for WinkitError {
    fn from(e: std::io::Error) -> Self {
        Self::new(ErrorKind::WindowsApiError, format!("I/O error: {e}")).with_source(e)
    }
}

/// Convenience alias used by tool handlers.
pub type ToolResult<T> = Result<T, WinkitError>;
