//! Service tools: read-only listing and lookup (§18). No modification.

use crate::errors::WinkitError;
use crate::permissions::Capability;
use crate::server::AppState;
use crate::tools::{clamp_limit, optional_usize, required_string, wrap, ToolDefinition};
use serde_json::{json, Value};
use std::sync::Arc;

pub async fn list_services_handler(
    state: Arc<AppState>,
    args: Value,
) -> Result<Value, WinkitError> {
    let max = state.config.limits.max_services;
    let limit = clamp_limit(optional_usize(&args, "limit"), max);
    let services = state.windows.list_services(limit)?;
    let count = services.len();
    let running = services.iter().filter(|s| s.state == "running").count();
    Ok(json!({
        "services": services,
        "count": count,
        "running": running,
        "truncated": count == limit,
    }))
}

pub fn list_services_definition() -> ToolDefinition {
    ToolDefinition {
        name: "list_services",
        description: "List Windows services (read-only): name, state, process ID.",
        input_schema: json!({
            "type": "object",
            "properties": {
                "limit": { "type": "integer", "minimum": 1, "description": "Maximum results (defaults to the configured limit)." },
            },
            "additionalProperties": false,
        }),
        capability: Some(Capability::ServiceRead),
        timeout_ms: None,
        handler: wrap(list_services_handler),
    }
}

pub async fn get_service_handler(state: Arc<AppState>, args: Value) -> Result<Value, WinkitError> {
    let name = required_string(&args, "name")?;
    match state.windows.get_service(&name)? {
        Some(service) => Ok(json!({ "service": service })),
        None => Err(WinkitError::invalid_argument(format!(
            "no service named '{name}' was found"
        ))),
    }
}

pub fn get_service_definition() -> ToolDefinition {
    ToolDefinition {
        name: "get_service",
        description: "Detailed read-only information about one Windows service by name, including binary path and start type.",
        input_schema: json!({
            "type": "object",
            "properties": {
                "name": { "type": "string", "description": "Service name (e.g. 'Spooler') or display name." },
            },
            "required": ["name"],
            "additionalProperties": false,
        }),
        capability: Some(Capability::ServiceRead),
        timeout_ms: None,
        handler: wrap(get_service_handler),
    }
}
