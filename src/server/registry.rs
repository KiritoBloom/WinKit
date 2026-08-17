//! Tool dispatch: permission enforcement, timeout, and payload limits.
//!
//! The MCP protocol layer calls [`call_tool`]; everything below (argument
//! validation, provider invocation, serialization bounds) lives here so the
//! protocol code stays thin.

use crate::errors::{ErrorKind, WinkitError};
use crate::server::AppState;
use crate::utils::limits::encode_limited;
use serde_json::Value;
use std::sync::Arc;

/// Look up a tool, enforce its capability, run it within its timeout, and
/// verify the serialized payload stays within the configured limit.
pub async fn call_tool(
    state: &Arc<AppState>,
    name: &str,
    args: Value,
) -> Result<Value, WinkitError> {
    let tool = state.tools.get(name).ok_or_else(|| {
        WinkitError::new(ErrorKind::InvalidArgument, format!("unknown tool '{name}'"))
    })?;

    if let Some(capability) = tool.capability {
        state.permissions.check(capability, name)?;
    }
    if let Some(action_capability) = crate::tools::tool_action_capability(name) {
        state.permissions.check_browser_action(
            action_capability,
            name,
            state.config.chrome.managed.enabled,
        )?;
    }

    let result = state.tools.call(state, name, args).await?;

    let _ = encode_limited(&result, state.config.limits.max_payload_bytes)?;
    Ok(result)
}

/// Map a `WinkitError` to the JSON-RPC error code the protocol emits,
/// attaching the stable WinKit error code in `data` so agents can match on
/// it without parsing message text.
///
/// The "unavailable" family (provider/application/feature/endpoint) maps to
/// distinct implementation-defined server errors in the `-32000..-32099`
/// range (JSON-RPC reserves this range for server errors) instead of
/// `-32603` (internal error), so a caller can tell "the chrome provider is
/// not enabled" from "something broke internally" without parsing the
/// message. `-32602` (invalid params) and `-32603` remain for the spec
/// cases.
pub fn map_protocol_error(err: &WinkitError) -> (i64, serde_json::Value) {
    let code = match err.kind {
        ErrorKind::InvalidArgument => -32602,          // INVALID_PARAMS
        ErrorKind::ProviderUnavailable => -32001,      // server error: provider not available
        ErrorKind::ApplicationUnavailable => -32002,   // server error: application unreachable
        ErrorKind::FeatureDisabled => -32003,          // server error: feature disabled in config
        ErrorKind::EndpointUnavailable => -32004,      // server error: endpoint unavailable
        ErrorKind::BrowserExited => -32005,            // server error: managed browser exited
        _ => -32603,                                   // INTERNAL_ERROR
    };
    (code, serde_json::json!({ "winkit_code": err.kind.code() }))
}
