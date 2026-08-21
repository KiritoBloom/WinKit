//! Process tools: listing, lookup, trees, and search.

use crate::errors::WinkitError;
use crate::permissions::Capability;
use crate::server::AppState;
use crate::tools::{
    clamp_limit, optional_string, optional_u32, optional_usize, wrap, ToolDefinition,
};
use serde_json::{json, Value};
use std::sync::Arc;

/// Longest `command_line` / `executable_path` string kept per row in list
/// output. Full untruncated values remain available via `get_process`.
const LIST_STRING_CAP: usize = 300;

fn sort_key(proc: &crate::models::ProcessInfo, key: &str) -> u64 {
    match key {
        "cpu_time" => proc.cpu_time_ms.unwrap_or(0),
        "pid" => proc.pid as u64,
        // Default and "memory" ordering: the backend already returns
        // memory-first; re-assert it here so explicit sort_by is stable.
        _ => proc.working_set_bytes.unwrap_or(0),
    }
}

pub async fn list_processes_handler(
    state: Arc<AppState>,
    args: Value,
) -> Result<Value, WinkitError> {
    let max = state.config.limits.max_processes;
    let limit = clamp_limit(optional_usize(&args, "limit"), max);
    let mut processes = state.windows.list_processes(limit)?;
    if let Some(sort_by) = optional_string(&args, "sort_by").as_deref() {
        let key = match sort_by {
            "memory" | "cpu_time" | "name" | "pid" => sort_by,
            other => {
                return Err(WinkitError::invalid_argument(format!(
                    "'sort_by' must be memory, cpu_time, name, or pid (got '{other}')"
                )))
            }
        };
        if key == "name" {
            processes.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
        } else {
            processes.sort_by_key(|p| std::cmp::Reverse(sort_key(p, key)));
        }
    }
    // `top` keeps only the first N rows after sorting (or as fetched).
    if let Some(top) = optional_usize(&args, "top") {
        processes.truncate(top.max(1));
    }
    // Bound long strings in list context to keep the payload compact;
    // `get_process` still returns full detail for a single PID.
    for p in processes.iter_mut() {
        if let Some(cl) = p.command_line.as_ref() {
            if cl.chars().count() > LIST_STRING_CAP {
                p.command_line = Some(crate::utils::truncate(cl, LIST_STRING_CAP));
            }
        }
    }
    let count = processes.len();
    Ok(json!({
        "processes": processes,
        "count": count,
        "truncated": count == limit,
        "command_line_capped_at": LIST_STRING_CAP,
    }))
}

pub fn list_processes_definition() -> ToolDefinition {
    ToolDefinition {
        name: "list_processes",
        description: "List running processes with PID, name, memory, threads, and start time. Processes with readable memory are listed first, ordered by memory usage. For big listings use sort_by (memory|cpu_time|name|pid) plus top (e.g. top 15) instead of paging through hundreds of rows; use get_process for full command lines of one PID.",
        input_schema: json!({
            "type": "object",
            "properties": {
                "limit": { "type": "integer", "minimum": 1, "description": "Maximum results (defaults to the configured limit)." },
                "sort_by": { "type": "string", "enum": ["memory", "cpu_time", "name", "pid"], "description": "Order results (default: memory)." },
                "top": { "type": "integer", "minimum": 1, "description": "Keep only the first N rows after sorting — the cheap way to ask 'top consumers'." },
            },
            "additionalProperties": false,
        }),
        capability: Some(Capability::ProcessRead),
        timeout_ms: None,
        handler: wrap(list_processes_handler),
    }
}

pub async fn get_process_handler(state: Arc<AppState>, args: Value) -> Result<Value, WinkitError> {
    let pid = crate::tools::parse_pid(&args, "pid")?;
    match state.windows.get_process(pid)? {
        Some(process) => Ok(crate::tools::item_envelope("process", json!(process))),
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
    let pid = crate::tools::parse_pid(&args, "pid")?;
    let max_depth = optional_u32(&args, "max_depth")
        .map(|d| d.clamp(1, state.config.limits.max_find_depth))
        .unwrap_or(state.config.limits.max_find_depth);
    const MAX_TREE_NODES: usize = 2000;
    let max_nodes = clamp_limit(optional_usize(&args, "max_nodes"), MAX_TREE_NODES);
    match state.windows.get_process_tree(pid, max_depth, max_nodes)? {
        Some(tree) => {
            Ok(json!({ "root": tree, "pid": pid, "max_depth": max_depth, "max_nodes": max_nodes }))
        }
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
    let name = name.trim();
    if name.is_empty() {
        return Err(WinkitError::invalid_argument("'name' must not be empty"));
    }
    if name.len() > 256 {
        return Err(WinkitError::invalid_argument(
            "'name' is too long (max 256)",
        ));
    }
    let max = state.config.limits.max_processes;
    let limit = clamp_limit(optional_usize(&args, "limit"), max);
    let processes = state.windows.find_process(name, limit)?;
    let count = processes.len();
    Ok(crate::tools::list_envelope(
        "processes",
        json!(processes),
        count,
        limit,
    ))
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;
    use crate::providers::mock::MockWindowsBackend;
    use crate::providers::windows::WindowsBackend;
    use crate::server::AppState;
    use serde_json::json;
    use std::sync::Arc;

    fn state() -> Arc<AppState> {
        let backend: Arc<dyn WindowsBackend> = Arc::new(MockWindowsBackend::with_fixtures());
        AppState::with_backend(Config::default(), backend).unwrap()
    }

    #[tokio::test]
    async fn list_processes_sort_by_and_top() {
        let out = list_processes_handler(state(), json!({ "sort_by": "memory", "top": 2 }))
            .await
            .unwrap();
        assert_eq!(out["count"], 2);
        let rows = out["processes"].as_array().unwrap();
        let mem: Vec<u64> = rows
            .iter()
            .map(|p| p["working_set_bytes"].as_u64().unwrap_or(0))
            .collect();
        assert!(mem.windows(2).all(|w| w[0] >= w[1]), "must be memory-descending");

        let out = list_processes_handler(state(), json!({ "sort_by": "name", "top": 3 }))
            .await
            .unwrap();
        let names: Vec<String> = out["processes"]
            .as_array()
            .unwrap()
            .iter()
            .map(|p| p["name"].as_str().unwrap().to_lowercase())
            .collect();
        let mut sorted = names.clone();
        sorted.sort();
        assert_eq!(names, sorted);
    }

    #[tokio::test]
    async fn list_processes_rejects_unknown_sort_key() {
        let err = list_processes_handler(state(), json!({ "sort_by": "magic" }))
            .await
            .unwrap_err();
        assert_eq!(err.kind, crate::errors::ErrorKind::InvalidArgument);
    }

    #[tokio::test]
    async fn list_processes_reports_command_line_cap() {
        let out = list_processes_handler(state(), json!({})).await.unwrap();
        assert!(out["command_line_capped_at"].as_u64().unwrap() > 0);
        assert!(out["truncated"].is_boolean());
    }
}
