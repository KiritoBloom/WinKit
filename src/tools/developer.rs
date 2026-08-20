//! Developer environment: structured info for coding agents.

use crate::errors::WinkitError;
use crate::permissions::Capability;
use crate::server::AppState;
use crate::tools::{wrap, ToolDefinition};
use serde_json::{json, Value};
use std::sync::Arc;

pub async fn dev_environment_handler(
    state: Arc<AppState>,
    _args: Value,
) -> Result<Value, WinkitError> {
    let environment = state.windows.dev_environment()?;
    Ok(json!({ "environment": environment }))
}

pub fn dev_environment_definition() -> ToolDefinition {
    ToolDefinition {
        name: "dev_environment",
        description: "Detect development tools (node, npm, cargo, docker, ...) on PATH and summarize well-known development servers. Nothing is installed or modified.",
        input_schema: json!({ "type": "object", "properties": {}, "additionalProperties": false }),
        capability: Some(Capability::ProcessRead),
        timeout_ms: None,
        handler: wrap(dev_environment_handler),
    }
}
