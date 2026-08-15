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
pub fn map_protocol_error(err: &WinkitError) -> (i64, serde_json::Value) {
    let code = match err.kind {
        ErrorKind::InvalidArgument => -32602, // INVALID_PARAMS
        _ => -32603,                          // INTERNAL_ERROR
    };
    (code, serde_json::json!({ "winkit_code": err.kind.code() }))
}
