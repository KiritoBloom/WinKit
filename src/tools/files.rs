//! File tools: bounded, read-only text reads, filename search, and
//! directory size breakdowns.
//!
//! These fill the biggest observability gap an agent has on Windows: the
//! ability to read a log or config file, locate a file by name, and answer
//! "what is eating space in this folder?" — all without any write path.
//!
//! Path policy reuses `workspaces.allow_roots` / `workspaces.deny_roots`
//! (empty allow list means "any absolute path"), and drive-root scans are
//! rejected unless explicitly allowed, matching the workspace tools.

use crate::errors::WinkitError;
use crate::permissions::Capability;
use crate::server::AppState;
use crate::tools::{
    optional_non_empty_string, optional_u32, optional_usize, required_string, validated_path, wrap,
    ToolDefinition,
};
use crate::utils::filesys::{
    self, DEFAULT_FIND_DEPTH, DEFAULT_READ_BYTES, MAX_DIR_WALK_ENTRIES, MAX_FIND_DEPTH,
    MAX_FIND_RESULTS, MAX_READ_BYTES,
};
use crate::utils::workspace;
use serde_json::{json, Value};
use std::path::Path;
use std::sync::Arc;

/// Validate a single-file read target: absolute, exists, regular file, and
/// inside the configured allow/deny policy. Returns the canonical path.
fn canonicalize_file(
    raw: &str,
    allow_roots: &[String],
    deny_roots: &[String],
) -> Result<std::path::PathBuf, WinkitError> {
    let raw = raw.trim();
    if raw.is_empty() {
        return Err(WinkitError::path_rejected("file path is empty"));
    }
    let path = Path::new(raw);
    if !path.is_absolute() {
        return Err(WinkitError::path_rejected(
            "file path must be absolute (e.g. D:\\logs\\app.log)",
        ));
    }
    let canonical = std::fs::canonicalize(path).map_err(|_| {
        WinkitError::not_found(format!("file '{raw}' does not exist or cannot be resolved"))
    })?;
    if !canonical.is_file() {
        return Err(WinkitError::invalid_argument(format!(
            "'{raw}' is not a regular file"
        )));
    }
    for deny in deny_roots {
        if let Ok(denied) = std::fs::canonicalize(deny) {
            if canonical == denied || canonical.starts_with(&denied) {
                return Err(WinkitError::path_rejected(format!(
                    "file '{raw}' is under a configured workspaces.deny_roots entry"
                )));
            }
        }
    }
    if !allow_roots.is_empty() {
        let allowed = allow_roots.iter().any(|r| {
            std::fs::canonicalize(r)
                .map(|rc| canonical.starts_with(&rc))
                .unwrap_or(false)
        });
        if !allowed {
            return Err(WinkitError::path_rejected(format!(
                "file '{raw}' is outside the configured workspaces.allow_roots"
            )));
        }
    }
    Ok(canonical)
}

// ---------------------------------------------------------------------------
// read_text_file
// ---------------------------------------------------------------------------

pub async fn read_text_file_handler(
    _state: Arc<AppState>,
    args: Value,
) -> Result<Value, WinkitError> {
    let raw = required_string(&args, "path")?;
    // Length bound before touching the filesystem (same cap as workspace paths).
    if raw.len() > 4096 {
        return Err(WinkitError::path_rejected("file path is too long"));
    }
    let mode: &'static str = match optional_non_empty_string(&args, "mode").as_deref() {
        None => "head",
        Some("head") => "head",
        Some("tail") => "tail",
        Some("all") => "all",
        Some(other) => {
            return Err(WinkitError::invalid_argument(format!(
                "'mode' must be head, tail, or all (got '{other}')"
            )))
        }
    };
    let max_bytes = optional_usize(&args, "max_bytes")
        .unwrap_or(DEFAULT_READ_BYTES)
        .clamp(64, MAX_READ_BYTES);
    let canonical = canonicalize_file(
        &validated_path(&raw)?,
        &_state.config.workspaces.allow_roots,
        &_state.config.workspaces.deny_roots,
    )?;
    let out =
        tokio::task::spawn_blocking(move || filesys::read_text_slice(&canonical, mode, max_bytes))
            .await
            .map_err(|e| WinkitError::internal(format!("read task failed: {e}")))??;
    Ok(json!({
        "path": raw.trim(),
        "mode": out.mode,
        "encoding": out.encoding,
        "total_bytes": out.total_bytes,
        "returned_bytes": out.returned_bytes,
        "truncated": out.truncated,
        "content": out.content,
    }))
}

pub fn read_text_file_definition() -> ToolDefinition {
    ToolDefinition {
        name: "read_text_file",
        description: "Read text from a file — logs, configs, manifests — without leaving the tool. Bounded to max_bytes (default 32 KB, cap 256 KB). Use mode=head to see the start of a file, mode=tail for the end of a log (whole-line aligned), mode=all for small files. UTF-8/UTF-16 BOMs are honored; binary files are refused. Read-only; honors workspaces.allow_roots/deny_roots.",
        input_schema: json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "description": "Absolute path to a text file." },
                "mode": { "type": "string", "enum": ["head", "tail", "all"], "description": "Which part to read (default head)." },
                "max_bytes": { "type": "integer", "minimum": 64, "maximum": 262144, "description": "Byte window (default 32768)." },
            },
            "required": ["path"],
            "additionalProperties": false,
        }),
        capability: Some(Capability::FilesystemRead),
        timeout_ms: None,
        handler: wrap(read_text_file_handler),
    }
}

// ---------------------------------------------------------------------------
// find_files
// ---------------------------------------------------------------------------

const DEFAULT_FIND_RESULTS: usize = 100;

pub async fn find_files_handler(state: Arc<AppState>, args: Value) -> Result<Value, WinkitError> {
    let root_raw = required_string(&args, "root")?;
    let pattern = required_string(&args, "pattern")?;
    if pattern.contains(['/', '\\']) {
        return Err(WinkitError::invalid_argument(
            "'pattern' matches file names only and must not contain path separators",
        ));
    }
    let max_depth = optional_u32(&args, "max_depth")
        .unwrap_or(DEFAULT_FIND_DEPTH)
        .clamp(0, MAX_FIND_DEPTH);
    let max_results = optional_usize(&args, "max_results")
        .map(|v| v.clamp(1, MAX_FIND_RESULTS))
        .unwrap_or(DEFAULT_FIND_RESULTS);

    let canonical = workspace::canonicalize_workspace(
        &root_raw,
        &state.config.workspaces.allow_roots,
        &state.config.workspaces.deny_roots,
    )
    .map_err(|e| WinkitError::path_rejected(e.message.replace("workspace path", "search root")))?;
    let pattern_for_reply = pattern.clone();

    let outcome = tokio::task::spawn_blocking(move || {
        filesys::walk_files(&canonical, max_depth, max_results, Some(&pattern))
    })
    .await
    .map_err(|e| WinkitError::internal(format!("walk task failed: {e}")))?;
    let count = outcome.files.len();
    Ok(json!({
        "root": root_raw.trim(),
        "pattern": pattern_for_reply,
        "files": outcome.files,
        "count": count,
        "truncated": outcome.truncated,
        "scanned_dirs": outcome.scanned_dirs,
        "unreadable_dirs": outcome.unreadable_dirs,
    }))
}

pub fn find_files_definition() -> ToolDefinition {
    ToolDefinition {
        name: "find_files",
        description: "Find files by name under a directory: case-insensitive wildcard pattern (* and ?), e.g. pattern=\"*.log\" or \"settings*.json\". Bounded depth (default 6) and results (default 100); never follows symlinks/junctions; reports truncated/unreadable_dirs instead of failing. Use it to locate configs, logs, build outputs. Read-only.",
        input_schema: json!({
            "type": "object",
            "properties": {
                "root": { "type": "string", "description": "Absolute directory to search under." },
                "pattern": { "type": "string", "description": "File-name wildcard, e.g. \"*.log\", \"*.pem\", \"report??.json\"." },
                "max_depth": { "type": "integer", "minimum": 0, "maximum": 12, "description": "Recursion depth below root (default 6)." },
                "max_results": { "type": "integer", "minimum": 1, "maximum": 500, "description": "Result cap (default 100)." },
            },
            "required": ["root", "pattern"],
            "additionalProperties": false,
        }),
        capability: Some(Capability::FilesystemRead),
        timeout_ms: None,
        handler: wrap(find_files_handler),
    }
}

// ---------------------------------------------------------------------------
// directory_overview
// ---------------------------------------------------------------------------

pub async fn directory_overview_handler(
    state: Arc<AppState>,
    args: Value,
) -> Result<Value, WinkitError> {
    let raw = required_string(&args, "path")?;
    let max_children = optional_usize(&args, "max_children")
        .unwrap_or(50)
        .clamp(1, 500);
    let canonical = workspace::canonicalize_workspace(
        &raw,
        &state.config.workspaces.allow_roots,
        &state.config.workspaces.deny_roots,
    )
    .map_err(|e| WinkitError::path_rejected(e.message.replace("workspace path", "directory")))?;

    let children = tokio::task::spawn_blocking(move || {
        let mut entries: Vec<(String, bool)> = Vec::new();
        if let Ok(read) = std::fs::read_dir(&canonical) {
            for e in read.flatten() {
                let Ok(meta) = e.metadata() else { continue };
                entries.push((e.path().to_string_lossy().into_owned(), meta.is_dir()));
            }
        }
        // Budget shared across children so one huge tree cannot starve the rest.
        let per_child_budget = (MAX_DIR_WALK_ENTRIES / entries.len().max(1)).max(2_000);
        let mut stats = Vec::new();
        let mut budget_used = 0usize;
        let mut global_truncated = false;
        for (path, is_dir) in entries {
            if is_dir {
                let remaining = per_child_budget.saturating_sub(budget_used.min(per_child_budget));
                let (s, used) = filesys::tree_stats(Path::new(&path), remaining);
                budget_used += used;
                if s.truncated {
                    global_truncated = true;
                }
                stats.push(s);
            } else if let Ok(meta) = std::fs::symlink_metadata(&path) {
                stats.push(filesys::TreeStats {
                    path: filesys::display_path(Path::new(&path)),
                    size_bytes: meta.len(),
                    files: 1,
                    dirs: 0,
                    truncated: false,
                });
            }
        }
        stats.sort_by(|a, b| b.size_bytes.cmp(&a.size_bytes));
        let total_entries = stats.iter().map(|s| s.files).sum::<usize>();
        let total_bytes = stats.iter().map(|s| s.size_bytes).sum::<u64>();
        (stats, total_entries, total_bytes, global_truncated)
    })
    .await
    .map_err(|e| WinkitError::internal(format!("overview task failed: {e}")))?;

    let (mut stats, total_files, total_bytes, walk_truncated) = children;
    let full_count = stats.len();
    stats.truncate(max_children);
    let truncated_children = walk_truncated || full_count > stats.len();
    Ok(json!({
        "path": raw.trim(),
        "total_size_bytes": total_bytes,
        "total_files": total_files,
        "children_shown": stats.len(),
        "children_truncated": truncated_children,
        "children": stats,
    }))
}

pub fn directory_overview_definition() -> ToolDefinition {
    ToolDefinition {
        name: "directory_overview",
        description: "Break a directory down by size: every child with its recursive on-disk size, file count, and subdirectory count, sorted largest-first. Answers \"what is eating disk space here?\" in one call. Walks are entry-budgeted and never follow junctions. Read-only.",
        input_schema: json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "description": "Absolute directory to break down." },
                "max_children": { "type": "integer", "minimum": 1, "maximum": 500, "description": "Children to include, sorted by size (default 50)." },
            },
            "required": ["path"],
            "additionalProperties": false,
        }),
        capability: Some(Capability::FilesystemRead),
        timeout_ms: None,
        handler: wrap(directory_overview_handler),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;
    use crate::providers::mock::MockWindowsBackend;
    use crate::providers::windows::WindowsBackend;
    use crate::server::AppState;
    use std::io::Write;

    fn state() -> Arc<AppState> {
        let backend: Arc<dyn WindowsBackend> = Arc::new(MockWindowsBackend::with_fixtures());
        AppState::with_backend(Config::default(), backend).unwrap()
    }

    /// Unique temp dir for this test binary run.
    fn temp_root(tag: &str) -> std::path::PathBuf {
        let dir =
            std::env::temp_dir().join(format!("winkit-file-tools-{tag}-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[tokio::test]
    async fn read_text_file_head_and_tail() {
        let dir = temp_root("read");
        let path = dir.join("app.log");
        let mut f = std::fs::File::create(&path).unwrap();
        for i in 0..500 {
            writeln!(f, "2026-08-21 log line {i:04} some detail").unwrap();
        }
        drop(f);
        let p = path.to_string_lossy().into_owned();

        let out = read_text_file_handler(state(), json!({ "path": p, "max_bytes": 512 }))
            .await
            .unwrap();
        assert_eq!(out["mode"], "head");
        assert_eq!(out["truncated"], true);
        assert!(out["content"]
            .as_str()
            .unwrap()
            .starts_with("2026-08-21 log line 0000"));

        let out = read_text_file_handler(
            state(),
            json!({ "path": p, "mode": "tail", "max_bytes": 512 }),
        )
        .await
        .unwrap();
        assert_eq!(out["mode"], "tail");
        let content = out["content"].as_str().unwrap();
        assert!(content.trim_end().ends_with("0499 some detail"));

        let out = read_text_file_handler(state(), json!({ "path": p, "mode": "all" }))
            .await
            .unwrap();
        assert_eq!(out["truncated"], false);

        let err = read_text_file_handler(state(), json!({ "path": p, "mode": "middle" })).await;
        assert_eq!(
            err.unwrap_err().kind,
            crate::errors::ErrorKind::InvalidArgument
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn read_text_file_refuses_binary_and_missing() {
        let dir = temp_root("binary");
        let path = dir.join("blob.bin");
        std::fs::write(&path, [0x4D, 0x5A, 0x00, 0x90, 0x00, 0x01]).unwrap();
        let p = path.to_string_lossy().into_owned();
        let err = read_text_file_handler(state(), json!({ "path": p }))
            .await
            .unwrap_err();
        assert!(err.message.contains("binary"));

        let err = read_text_file_handler(
            state(),
            json!({ "path": dir.join("nope.txt").to_string_lossy().to_string() }),
        )
        .await
        .unwrap_err();
        assert_eq!(err.kind, crate::errors::ErrorKind::NotFound);

        let err = read_text_file_handler(state(), json!({ "path": "relative\\file.txt" }))
            .await
            .unwrap_err();
        assert_eq!(err.kind, crate::errors::ErrorKind::PathRejected);

        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn find_files_matches_wildcards_and_reports_truncation() {
        let dir = temp_root("find");
        let nested = dir.join("a").join("b");
        std::fs::create_dir_all(&nested).unwrap();
        std::fs::write(dir.join("alpha.log"), b"1").unwrap();
        std::fs::write(dir.join("beta.txt"), b"2").unwrap();
        std::fs::write(nested.join("gamma.log"), b"3").unwrap();

        let root = dir.to_string_lossy().into_owned();
        let out = find_files_handler(state(), json!({ "root": root, "pattern": "*.log" }))
            .await
            .unwrap();
        assert_eq!(out["count"], 2);
        assert_eq!(out["truncated"], false);
        assert!(out["files"]
            .as_array()
            .unwrap()
            .iter()
            .all(|f| f["path"].as_str().unwrap().ends_with(".log")));

        let err = find_files_handler(state(), json!({ "root": root, "pattern": "a/b" })).await;
        assert_eq!(
            err.unwrap_err().kind,
            crate::errors::ErrorKind::InvalidArgument
        );

        let err = find_files_handler(state(), json!({ "root": "C:\\", "pattern": "*" })).await;
        assert_eq!(
            err.unwrap_err().kind,
            crate::errors::ErrorKind::PathRejected
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn directory_overview_sorts_by_recursive_size() {
        let dir = temp_root("overview");
        let big = dir.join("big");
        let tiny = dir.join("tiny");
        std::fs::create_dir_all(&big).unwrap();
        std::fs::create_dir_all(&tiny).unwrap();
        std::fs::write(big.join("data.bin"), vec![9u8; 5000]).unwrap();
        std::fs::write(tiny.join("note.txt"), b"hi").unwrap();
        std::fs::write(dir.join("loose.bin"), vec![9u8; 100]).unwrap();

        let root = dir.to_string_lossy().into_owned();
        let out = directory_overview_handler(state(), json!({ "path": root }))
            .await
            .unwrap();
        let children = out["children"].as_array().unwrap();
        assert_eq!(children.len(), 3);
        assert_eq!(
            children[0]["path"].as_str().unwrap().replace('/', "\\"),
            big.to_string_lossy()
        );
        assert_eq!(children[0]["size_bytes"], 5000);
        // 5000 (big/data.bin) + 2 (tiny/note.txt) + 100 (loose.bin).
        let expected: u64 = 5102;
        assert_eq!(out["total_size_bytes"], expected);

        std::fs::remove_dir_all(&dir).ok();
    }
}
