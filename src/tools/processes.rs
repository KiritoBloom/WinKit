//! Process tools: listing, lookup, trees, and search (§15).

use crate::errors::WinkitError;
use crate::permissions::Capability;
use crate::server::AppState;
use crate::tools::{
    clamp_limit, optional_string, optional_u32, optional_usize, required_u32, wrap, ToolDefinition,
};
use serde_json::{json, Value};
use std::sync::Arc;

pub async fn list_processes_handler(
    state: Arc<AppState>,
    args: Value,
) -> Result<Value, WinkitError> {
    let max = state.config.limits.max_processes;
    let limit = clamp_limit(optional_usize(&args, "limit"), max);
    let processes = state.windows.list_processes(limit)?;
    let count = processes.len();
    Ok(json!({ "processes": processes, "count": count, "truncated": count == limit }))
}

pub fn list_processes_definition() -> ToolDefinition {
    ToolDefinition {
        name: "list_processes",
        description: "List running processes with PID, name, memory, threads, and start time. Processes with readable memory are listed first, ordered by memory usage.",
        input_schema: json!({
            "type": "object",
            "properties": {
                "limit": { "type": "integer", "minimum": 1, "description": "Maximum results (defaults to the configured limit)." },
            },
            "additionalProperties": false,
        }),
        capability: Some(Capability::ProcessRead),
        timeout_ms: None,
        handler: wrap(list_processes_handler),
    }
}

pub async fn get_process_handler(state: Arc<AppState>, args: Value) -> Result<Value, WinkitError> {
    let pid = required_u32(&args, "pid")?;
    match state.windows.get_process(pid)? {
        Some(process) => Ok(json!({ "process": process })),
        None => Err(WinkitError::invalid_argument(format!(
            "no process with pid {pid} is running"
        ))),
    }
}

pub fn get_process_definition() -> ToolDefinition {
    ToolDefinition {
        name: "get_process",
        description: "Detailed information about one process by PID: memory, threads, CPU time, executable path, and command line when readable. Includes a two-sample CPU percent estimate (relative to total system CPU capacity) when the process is openable; for multi-sample CPU evidence use system_health.",
        input_schema: json!({
            "type": "object",
            "properties": {
                "pid": { "type": "integer", "minimum": 1 },
            },
            "required": ["pid"],
            "additionalProperties": false,
        }),
        capability: Some(Capability::ProcessRead),
        timeout_ms: None,
        handler: wrap(get_process_handler),
    }
}

pub async fn get_process_tree_handler(
    state: Arc<AppState>,
    args: Value,
) -> Result<Value, WinkitError> {
    let pid = required_u32(&args, "pid")?;
    let max_depth = optional_u32(&args, "max_depth")
        .map(|d| d.min(state.config.limits.max_find_depth))
        .unwrap_or(state.config.limits.max_find_depth);
    let max_nodes = optional_usize(&args, "max_nodes").unwrap_or(500);
    match state.windows.get_process_tree(pid, max_depth, max_nodes)? {
        Some(tree) => Ok(json!({ "root": tree })),
        None => Err(WinkitError::invalid_argument(format!(
            "no process with pid {pid} is running"
        ))),
    }
}

pub fn get_process_tree_definition() -> ToolDefinition {
    ToolDefinition {
        name: "get_process_tree",
        description: "The process tree rooted at a PID (children and descendants), depth-bounded.",
        input_schema: json!({
            "type": "object",
            "properties": {
                "pid": { "type": "integer", "minimum": 1 },
                "max_depth": { "type": "integer", "minimum": 1, "maximum": 32, "description": "Default: configured max find depth." },
                "max_nodes": { "type": "integer", "minimum": 1, "default": 500 },
            },
            "required": ["pid"],
            "additionalProperties": false,
        }),
        capability: Some(Capability::ProcessRead),
        timeout_ms: None,
        handler: wrap(get_process_tree_handler),
    }
}

pub async fn find_process_handler(state: Arc<AppState>, args: Value) -> Result<Value, WinkitError> {
    let name = optional_string(&args, "name")
        .or_else(|| optional_string(&args, "query"))
        .ok_or_else(|| WinkitError::invalid_argument("missing required argument 'name'"))?;
    if name.trim().is_empty() {
        return Err(WinkitError::invalid_argument("'name' must not be empty"));
    }
    let max = state.config.limits.max_processes;
    let limit = clamp_limit(optional_usize(&args, "limit"), max);
    let processes = state.windows.find_process(&name, limit)?;
    let count = processes.len();
    Ok(json!({ "processes": processes, "count": count }))
}

pub fn find_process_definition() -> ToolDefinition {
    ToolDefinition {
        name: "find_process",
        description: "Search running processes by case-insensitive substring of the executable name (e.g. 'chrome', 'node').",
        input_schema: json!({
            "type": "object",
            "properties": {
                "name": { "type": "string", "description": "Substring to match against process names." },
                "limit": { "type": "integer", "minimum": 1 },
            },
            "required": ["name"],
            "additionalProperties": false,
        }),
        capability: Some(Capability::ProcessRead),
        timeout_ms: None,
        handler: wrap(find_process_handler),
    }
}
