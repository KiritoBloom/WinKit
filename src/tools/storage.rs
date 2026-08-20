//! Storage tools: drives, usage, and explicit-path large-file scans.

use crate::errors::WinkitError;
use crate::models::FindLargeFilesRequest;
use crate::permissions::Capability;
use crate::server::AppState;
use crate::tools::{
    clamp_limit, optional_string_array, optional_u32, optional_usize, required_path,
    validate_min_size_mb, wrap, ToolDefinition,
};
use serde_json::{json, Value};
use std::path::PathBuf;
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

pub async fn find_large_files_handler(
    state: Arc<AppState>,
    args: Value,
) -> Result<Value, WinkitError> {
    let path_str = required_path(&args, "path")?;
    let path = PathBuf::from(&path_str);
    if !path.is_dir() {
        return Err(WinkitError::invalid_argument(format!(
            "'path' must be an existing directory (got '{}')",
            path.display()
        )));
    }
    let max_depth = optional_u32(&args, "max_depth")
        .map(|d| d.clamp(1, state.config.limits.max_find_depth))
        .unwrap_or(state.config.limits.max_find_depth);
    let max_results = clamp_limit(
        optional_usize(&args, "max_results"),
        state.config.limits.max_storage_results,
    );
    let min_size_bytes = validate_min_size_mb(&args, "min_size_mb", 50.0)?;
    let extensions = optional_string_array(&args, "extensions");

    let request = FindLargeFilesRequest {
        path,
        min_size_bytes,
        max_depth,
        max_results,
        extensions,
    };
    let files = state.windows.find_large_files(request)?;
    let count = files.len();
    Ok(crate::tools::list_envelope(
        "files",
        json!(files),
        count,
        max_results,
    ))
}

pub fn find_large_files_definition() -> ToolDefinition {
    ToolDefinition {
        name: "find_large_files",
        description: "Find large files under an explicit directory. Requires an explicit path; never scans an entire drive.",
        input_schema: json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "description": "Directory to scan (must exist)." },
                "min_size_mb": { "type": "number", "minimum": 0, "description": "Minimum file size in MB (default 50)." },
                "max_depth": { "type": "integer", "minimum": 1, "maximum": 32, "description": "Recursion depth (default: configured limit)." },
                "max_results": { "type": "integer", "minimum": 1, "description": "Maximum results (default: configured limit)." },
                "extensions": { "type": "array", "items": { "type": "string" }, "description": "Optional extension filter, e.g. [\"log\", \"zip\"]." },
            },
            "required": ["path"],
            "additionalProperties": false,
        }),
        capability: Some(Capability::StorageRead),
        timeout_ms: None,
        handler: wrap(find_large_files_handler),
    }
}
