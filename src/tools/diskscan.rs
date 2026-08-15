//! Disk-space analysis tools (§ storage): whole-volume scan with an
//! NTFS metadata fast path, background scanning with cancellation, and
//! cheap snapshot queries (largest files, largest folders, folder size,
//! pattern find).
//!
//! All tools share one per-volume snapshot: `disk_scan` (or
//! `disk_scan_start`) builds it; the query tools reuse it in milliseconds.
//! Every result reports which scanner produced it (`scanner`), whether it
//! came from cache, and the snapshot age.

use crate::errors::WinkitError;
use crate::models::{DiskQueryKind, DiskQueryRequest, DiskQueryResult, DiskScanRequest};
use crate::permissions::Capability;
use crate::server::AppState;
use crate::tools::{
    clamp_limit, optional_bool, optional_f64, optional_string, optional_string_array, optional_u64,
    optional_usize, required_string, wrap, ToolDefinition,
};
use serde_json::{json, Value};
use std::sync::Arc;

/// Run a blocking backend call on a worker thread so a long first scan
/// never stalls the async runtime.
async fn spawn_block_call<F, T>(f: F) -> Result<T, WinkitError>
where
    F: FnOnce() -> Result<T, WinkitError> + Send + 'static,
    T: Send + 'static,
{
    tokio::task::spawn_blocking(f)
        .await
        .map_err(|e| WinkitError::internal(format!("background task failed: {e}")))?
}

fn diagnostics_json(
    volume: &str,
    scanner: &str,
    cached: bool,
    snapshot_age_ms: Option<u64>,
) -> Value {
    json!({
        "volume": volume,
        "scanner": scanner,
        "cached": cached,
        "snapshot_age_ms": snapshot_age_ms,
    })
}

// ---------------------------------------------------------------------------
// disk_scan (synchronous, cached)
// ---------------------------------------------------------------------------

pub async fn disk_scan_handler(state: Arc<AppState>, args: Value) -> Result<Value, WinkitError> {
    let path = required_string(&args, "path")?;
    let refresh = optional_bool(&args, "refresh").unwrap_or(false);
    let max_age_ms = optional_u64(&args, "max_age_ms").unwrap_or(0);
    let request = DiskScanRequest {
        path,
        refresh,
        max_age_ms,
    };
    let state = state.clone();
    let info = spawn_block_call(move || state.windows.disk_scan(&request)).await?;
    Ok(json!({ "scan": info }))
}

pub fn disk_scan_definition() -> ToolDefinition {
    ToolDefinition {
        name: "disk_scan",
        description: "Scan the volume containing a path and return a compact storage summary: capacity, indexed counts, largest files, and largest folders. Uses a fast NTFS MFT metadata scan when possible; when the fast path is unavailable (e.g. volume access denied without an elevated token) it falls back to a recursive scan of the requested directory and reports scanner='recursive_fallback' with the reason in fast_path_unavailable. Results are cached per scope, so repeated calls are instant. Use refresh=true to force a rescan.",
        input_schema: json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "description": "Any path on the volume to analyze, e.g. 'D:', 'D:\\', or 'D:\\Games'." },
                "refresh": { "type": "boolean", "description": "Force a fresh scan even when a cached snapshot is available (default false)." },
                "max_age_ms": { "type": "number", "description": "Serve a cached snapshot younger than this many ms without rescanning (default 30000)." },
            },
            "required": ["path"],
            "additionalProperties": false,
        }),
        capability: Some(Capability::StorageRead),
        timeout_ms: None,
        handler: wrap(disk_scan_handler),
    }
}

// ---------------------------------------------------------------------------
// disk_scan_start / disk_scan_status / disk_scan_cancel
// ---------------------------------------------------------------------------

pub async fn disk_scan_start_handler(
    state: Arc<AppState>,
    args: Value,
) -> Result<Value, WinkitError> {
    let path = required_string(&args, "path")?;
    let request = DiskScanRequest {
        path,
        refresh: true,
        max_age_ms: 0,
    };
    let state = state.clone();
    let status = spawn_block_call(move || state.windows.disk_scan_start(&request)).await?;
    Ok(json!({ "status": status }))
}

pub fn disk_scan_start_definition() -> ToolDefinition {
    ToolDefinition {
        name: "disk_scan_start",
        description: "Start a background scan of the volume containing a path. Returns a scan_id and status; poll with disk_scan_status. The scan does not block other MCP tools.",
        input_schema: json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "description": "Any path on the volume to analyze, e.g. 'D:'." },
            },
            "required": ["path"],
            "additionalProperties": false,
        }),
        capability: Some(Capability::StorageRead),
        timeout_ms: None,
        handler: wrap(disk_scan_start_handler),
    }
}

pub async fn disk_scan_status_handler(
    state: Arc<AppState>,
    args: Value,
) -> Result<Value, WinkitError> {
    let scan_id = required_string(&args, "scan_id")?;
    let status = state.windows.disk_scan_status(&scan_id)?;
    match status {
        Some(s) => Ok(json!({ "status": s })),
        None => Err(WinkitError::not_found(format!(
            "no scan with id '{scan_id}'"
        ))),
    }
}

pub fn disk_scan_status_definition() -> ToolDefinition {
    ToolDefinition {
        name: "disk_scan_status",
        description: "Poll a background disk scan by scan_id. When the scan completes, the result includes the full storage summary.",
        input_schema: json!({
            "type": "object",
            "properties": {
                "scan_id": { "type": "string", "description": "The scan_id returned by disk_scan_start." },
            },
            "required": ["scan_id"],
            "additionalProperties": false,
        }),
        capability: Some(Capability::StorageRead),
        timeout_ms: None,
        handler: wrap(disk_scan_status_handler),
    }
}

pub async fn disk_scan_cancel_handler(
    state: Arc<AppState>,
    args: Value,
) -> Result<Value, WinkitError> {
    let scan_id = required_string(&args, "scan_id")?;
    let cancelled = state.windows.disk_scan_cancel(&scan_id)?;
    Ok(json!({ "cancelled": cancelled, "scan_id": scan_id }))
}

pub fn disk_scan_cancel_definition() -> ToolDefinition {
    ToolDefinition {
        name: "disk_scan_cancel",
        description: "Cancel a running background disk scan by scan_id. The scan stops at the next checkpoint; partial results are discarded.",
        input_schema: json!({
            "type": "object",
            "properties": {
                "scan_id": { "type": "string", "description": "The scan_id returned by disk_scan_start." },
            },
            "required": ["scan_id"],
            "additionalProperties": false,
        }),
        capability: Some(Capability::StorageRead),
        timeout_ms: None,
        handler: wrap(disk_scan_cancel_handler),
    }
}

// ---------------------------------------------------------------------------
// Snapshot queries
// ---------------------------------------------------------------------------

pub async fn disk_scan_largest_files_handler(
    state: Arc<AppState>,
    args: Value,
) -> Result<Value, WinkitError> {
    let path = required_string(&args, "path")?;
    let limit = clamp_limit(
        optional_usize(&args, "limit"),
        state.config.limits.max_storage_results,
    );
    let min_size_bytes = optional_f64(&args, "min_size_mb")
        .map(|mb| (mb * 1024.0 * 1024.0) as u64)
        .unwrap_or(0);
    let extensions = optional_string_array(&args, "extensions");
    let request = DiskQueryRequest {
        path,
        kind: DiskQueryKind::TopFiles,
        limit,
        min_size_bytes,
        extensions,
        pattern: None,
    };
    match state.windows.disk_scan_query(&request)? {
        DiskQueryResult::TopFiles {
            entries,
            volume,
            scanner,
            cached,
            snapshot_age_ms,
        } => {
            let count = entries.len();
            Ok(json!({
                "files": entries,
                "count": count,
                "truncated": count == limit,
                "diagnostics": diagnostics_json(&volume, &scanner, cached, snapshot_age_ms),
            }))
        }
        _ => Err(WinkitError::internal("unexpected query result kind")),
    }
}

pub fn disk_scan_largest_files_definition() -> ToolDefinition {
    ToolDefinition {
        name: "disk_scan_largest_files",
        description: "Largest files on the volume containing a path, or under that path when it is a directory. Answered from the cached snapshot in milliseconds (a scan happens automatically on first use).",
        input_schema: json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "description": "A path on the volume (e.g. 'D:') or a subtree root (e.g. 'D:\\Games')." },
                "limit": { "type": "integer", "minimum": 1, "description": "Maximum results (default: configured limit)." },
                "min_size_mb": { "type": "number", "minimum": 0, "description": "Only files at least this large, in MB (default 0)." },
                "extensions": { "type": "array", "items": { "type": "string" }, "description": "Optional extension filter, e.g. [\"zip\", \"iso\"]." },
            },
            "required": ["path"],
            "additionalProperties": false,
        }),
        capability: Some(Capability::StorageRead),
        timeout_ms: None,
        handler: wrap(disk_scan_largest_files_handler),
    }
}

pub async fn disk_scan_largest_folders_handler(
    state: Arc<AppState>,
    args: Value,
) -> Result<Value, WinkitError> {
    let path = required_string(&args, "path")?;
    let limit = clamp_limit(
        optional_usize(&args, "limit"),
        state.config.limits.max_storage_results,
    );
    let request = DiskQueryRequest {
        path,
        kind: DiskQueryKind::TopFolders,
        limit,
        min_size_bytes: 0,
        extensions: None,
        pattern: None,
    };
    match state.windows.disk_scan_query(&request)? {
        DiskQueryResult::TopFolders {
            entries,
            volume,
            scanner,
            cached,
            snapshot_age_ms,
        } => {
            let count = entries.len();
            Ok(json!({
                "folders": entries,
                "count": count,
                "truncated": count == limit,
                "diagnostics": diagnostics_json(&volume, &scanner, cached, snapshot_age_ms),
            }))
        }
        _ => Err(WinkitError::internal("unexpected query result kind")),
    }
}

pub fn disk_scan_largest_folders_definition() -> ToolDefinition {
    ToolDefinition {
        name: "disk_scan_largest_folders",
        description: "Largest directories on the volume containing a path, or under that path when it is a directory. Sizes are aggregate descendant-file sizes. Answered from the cached snapshot.",
        input_schema: json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "description": "A path on the volume (e.g. 'D:') or a subtree root (e.g. 'D:\\Games')." },
                "limit": { "type": "integer", "minimum": 1, "description": "Maximum results (default: configured limit)." },
            },
            "required": ["path"],
            "additionalProperties": false,
        }),
        capability: Some(Capability::StorageRead),
        timeout_ms: None,
        handler: wrap(disk_scan_largest_folders_handler),
    }
}

pub async fn disk_scan_folder_size_handler(
    state: Arc<AppState>,
    args: Value,
) -> Result<Value, WinkitError> {
    let path = required_string(&args, "path")?;
    let request = DiskQueryRequest {
        path,
        kind: DiskQueryKind::FolderSize,
        limit: 0,
        min_size_bytes: 0,
        extensions: None,
        pattern: None,
    };
    match state.windows.disk_scan_query(&request)? {
        DiskQueryResult::FolderSize {
            folder,
            volume,
            scanner,
            cached,
            snapshot_age_ms,
        } => Ok(json!({
            "folder": folder,
            "diagnostics": diagnostics_json(&volume, &scanner, cached, snapshot_age_ms),
        })),
        _ => Err(WinkitError::internal("unexpected query result kind")),
    }
}

pub fn disk_scan_folder_size_definition() -> ToolDefinition {
    ToolDefinition {
        name: "disk_scan_folder_size",
        description: "Aggregate size of a directory (sum of descendant file sizes), answered from the cached snapshot without rescanning.",
        input_schema: json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "description": "The directory to measure, e.g. 'D:\\Games'." },
            },
            "required": ["path"],
            "additionalProperties": false,
        }),
        capability: Some(Capability::StorageRead),
        timeout_ms: None,
        handler: wrap(disk_scan_folder_size_handler),
    }
}

pub async fn disk_scan_find_handler(
    state: Arc<AppState>,
    args: Value,
) -> Result<Value, WinkitError> {
    let path = required_string(&args, "path")?;
    let pattern = optional_string(&args, "pattern");
    let limit = clamp_limit(
        optional_usize(&args, "limit"),
        state.config.limits.max_storage_results,
    );
    let min_size_bytes = optional_f64(&args, "min_size_mb")
        .map(|mb| (mb * 1024.0 * 1024.0) as u64)
        .unwrap_or(0);
    let extensions = optional_string_array(&args, "extensions");
    let request = DiskQueryRequest {
        path,
        kind: DiskQueryKind::FindFiles,
        limit,
        min_size_bytes,
        extensions,
        pattern,
    };
    match state.windows.disk_scan_query(&request)? {
        DiskQueryResult::FindFiles {
            entries,
            truncated,
            volume,
            scanner,
            cached,
            snapshot_age_ms,
        } => {
            let count = entries.len();
            Ok(json!({
                "files": entries,
                "count": count,
                "truncated": truncated,
                "diagnostics": diagnostics_json(&volume, &scanner, cached, snapshot_age_ms),
            }))
        }
        _ => Err(WinkitError::internal("unexpected query result kind")),
    }
}

pub fn disk_scan_find_definition() -> ToolDefinition {
    ToolDefinition {
        name: "disk_scan_find",
        description: "Find files under a path matching a name pattern ('*' and '?' wildcards, or a substring), an extension filter, and a minimum size. Answered from the cached snapshot; full paths are only built for matches.",
        input_schema: json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "description": "Directory to search under, e.g. 'D:\\Projects'." },
                "pattern": { "type": "string", "description": "Name pattern: '*.zip', '?ata*', or a plain substring." },
                "extensions": { "type": "array", "items": { "type": "string" }, "description": "Optional extension filter." },
                "min_size_mb": { "type": "number", "minimum": 0, "description": "Only files at least this large, in MB (default 0)." },
                "limit": { "type": "integer", "minimum": 1, "description": "Maximum results (default: configured limit)." },
            },
            "required": ["path"],
            "additionalProperties": false,
        }),
        capability: Some(Capability::StorageRead),
        timeout_ms: None,
        handler: wrap(disk_scan_find_handler),
    }
}
