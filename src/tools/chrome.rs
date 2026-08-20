//! Shared Chrome application helpers — single source for provider lookup, timeout, and schemas.

use crate::config::Config;
use crate::errors::WinkitError;
use crate::providers::applications::ApplicationProvider;
use crate::server::AppState;
use serde_json::{json, Value};

pub fn chrome_provider(state: &AppState) -> Result<&dyn ApplicationProvider, WinkitError> {
    state.applications.get("chrome").ok_or_else(|| {
        WinkitError::provider_unavailable(
            "the chrome provider is not enabled in this configuration",
        )
    })
}

pub fn chrome_timeout(config: &Config) -> Option<u64> {
    Some(config.chrome.operation_timeout_ms)
}

pub fn tab_id_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "tab_id": { "type": "string", "description": "Tab target id (from chrome_list_tabs) or exact URL." },
        },
        "required": ["tab_id"],
        "additionalProperties": false,
    })
}
