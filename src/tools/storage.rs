//! Storage tools: drives and per-volume usage (read-only, no tree walks).

use crate::errors::WinkitError;
use crate::permissions::Capability;
use crate::server::AppState;
use crate::tools::{required_path, wrap, ToolDefinition};
use serde_json::{json, Value};
use std::sync::Arc;

pub async fn list_drives_handler(state: Arc<AppState>, _args: Value) -> Result<Value, WinkitError> {
    let drives = state.windows.list_drives()?;
    let count = drives.len();
    Ok(crate::tools::list_envelope(
        "drives",
        json!(drives),
        count,
        count,
    ))
}

pub fn list_drives_definition() -> ToolDefinition {
    ToolDefinition {
        name: "list_drives",
        description: "List storage volumes with type, capacity, and free space.",
        input_schema: json!({ "type": "object", "properties": {}, "additionalProperties": false }),
        capability: Some(Capability::StorageRead),
        timeout_ms: None,
        handler: wrap(list_drives_handler),
    }
}

pub async fn disk_usage_handler(state: Arc<AppState>, args: Value) -> Result<Value, WinkitError> {
    let path = required_path(&args, "path")?;
    let usage = state.windows.disk_usage(&path)?;
    Ok(json!({ "path": path, "usage": usage }))
}

pub fn disk_usage_definition() -> ToolDefinition {
    ToolDefinition {
        name: "disk_usage",
        description: "Free/used space for the volume containing a path.",
        input_schema: json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "description": "Any path on the volume to inspect (e.g. 'C:\\' or 'C:\\Users')." },
            },
            "required": ["path"],
            "additionalProperties": false,
        }),
        capability: Some(Capability::StorageRead),
        timeout_ms: None,
        handler: wrap(disk_usage_handler),
    }
}
