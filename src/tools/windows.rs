//! Window tools: read-only listing.

use crate::errors::WinkitError;
use crate::permissions::Capability;
use crate::server::AppState;
use crate::tools::{clamp_limit, optional_bool, optional_usize, wrap, ToolDefinition};
use serde_json::{json, Value};
use std::sync::Arc;

pub async fn list_windows_handler(state: Arc<AppState>, args: Value) -> Result<Value, WinkitError> {
    let max = state.config.limits.max_windows;
    let limit = clamp_limit(optional_usize(&args, "limit"), max);
    let include_hidden = optional_bool(&args, "include_hidden").unwrap_or(false);
    let windows = state.windows.list_windows(limit, !include_hidden)?;
    let count = windows.len();
    Ok(crate::tools::list_envelope(
        "windows",
        json!(windows),
        count,
        limit,
    ))
}

pub fn list_windows_definition() -> ToolDefinition {
    ToolDefinition {
        name: "list_windows",
        description:
            "List visible top-level windows with title, owning process, and foreground state.",
        input_schema: json!({
            "type": "object",
            "properties": {
                "limit": { "type": "integer", "minimum": 1, "description": "Maximum results (defaults to the configured limit)." },
                "include_hidden": { "type": "boolean", "description": "Also include hidden windows (default false)." },
            },
            "additionalProperties": false,
        }),
        capability: Some(Capability::WindowRead),
        timeout_ms: None,
        handler: wrap(list_windows_handler),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;
    use crate::models::WindowInfo;
    use crate::providers::mock::MockWindowsBackend;
    use crate::providers::windows::WindowsBackend;
    use crate::server::registry;
    use serde_json::json;
    use std::sync::Arc;

    fn window(
        hwnd: isize,
        title: &str,
        pid: u32,
        process_name: Option<&str>,
        visible: bool,
    ) -> WindowInfo {
        WindowInfo {
            hwnd,
            title: title.to_string(),
            class_name: Some("MockWindowClass".to_string()),
            process_id: pid,
            process_name: process_name.map(str::to_string),
            visible,
            minimized: false,
            maximized: false,
            foreground: false,
        }
    }

    fn state_with(windows: Vec<WindowInfo>) -> Arc<AppState> {
        let backend: Arc<dyn WindowsBackend> = Arc::new(MockWindowsBackend {
            windows,
            ..Default::default()
        });
        let mut config = Config::default();
        config.permissions.mode = "read_only".to_string();
        config.providers.enabled = vec!["windows".to_string()];
        AppState::with_backend(config, backend).unwrap()
    }

    async fn call(state: &Arc<AppState>, args: Value) -> Result<Value, WinkitError> {
        registry::call_tool(state, "list_windows", args).await
    }

    /// Three visible windows (one with an empty title) and one hidden window.
    fn sample_windows() -> Vec<WindowInfo> {
        vec![
            window(1, "ChatGPT", 1001, Some("chrome.exe"), true),
            window(2, "OpenCode", 1002, Some("opencode.exe"), true),
            window(3, "", 1003, Some("explorer.exe"), true),
            window(4, "Hidden Helper", 1004, None, false),
        ]
    }

    #[tokio::test]
    async fn visible_only_returns_visible_windows_with_matching_count() {
        let state = state_with(sample_windows());
        let out = call(&state, json!({})).await.unwrap();
        let windows = out["windows"].as_array().unwrap();
        assert_eq!(windows.len(), 3);
        assert_eq!(out["count"], windows.len());
        assert!(windows.iter().all(|w| w["visible"].as_bool().unwrap()));
        let titles: Vec<&str> = windows.iter().filter_map(|w| w["title"].as_str()).collect();
        assert!(!titles.contains(&"Hidden Helper"));
    }

    #[tokio::test]
    async fn include_hidden_returns_visible_and_hidden() {
        let state = state_with(sample_windows());
        let out = call(&state, json!({ "include_hidden": true }))
            .await
            .unwrap();
        let windows = out["windows"].as_array().unwrap();
        assert_eq!(windows.len(), 4);
        assert_eq!(out["count"], windows.len());
        assert!(windows
            .iter()
            .any(|w| w["visible"].as_bool() == Some(false)));
    }

    #[tokio::test]
    async fn include_hidden_defaults_to_false() {
        let state = state_with(sample_windows());
        let out = call(&state, json!({})).await.unwrap();
        let windows = out["windows"].as_array().unwrap();
        assert_eq!(windows.len(), 3);
        assert!(windows.iter().all(|w| w["visible"].as_bool().unwrap()));
    }

    #[tokio::test]
    async fn empty_desktop_returns_empty_without_error() {
        let state = state_with(Vec::new());
        let out = call(&state, json!({})).await.unwrap();
        assert_eq!(out["count"], 0);
        assert!(out["windows"].as_array().unwrap().is_empty());
    }

    #[tokio::test]
    async fn empty_title_window_is_still_returned() {
        let state = state_with(vec![window(1, "", 1001, Some("chrome.exe"), true)]);
        let out = call(&state, json!({})).await.unwrap();
        let windows = out["windows"].as_array().unwrap();
        assert_eq!(windows.len(), 1);
        assert_eq!(windows[0]["title"], "");
    }

    #[tokio::test]
    async fn unresolvable_process_name_is_null_not_an_error() {
        let state = state_with(vec![window(1, "No Meta", 9999, None, true)]);
        let out = call(&state, json!({})).await.unwrap();
        let windows = out["windows"].as_array().unwrap();
        assert_eq!(windows.len(), 1);
        assert!(windows[0]["process_name"].is_null());
    }

    #[tokio::test]
    async fn limit_zero_clamps_to_at_least_one() {
        let state = state_with(sample_windows());
        let out = call(&state, json!({ "limit": 0 })).await.unwrap();
        let windows = out["windows"].as_array().unwrap();
        assert_eq!(windows.len(), 1);
        assert_eq!(out["count"], 1);
    }

    #[tokio::test]
    async fn limit_truncates_and_count_matches() {
        let state = state_with(sample_windows());
        let out = call(&state, json!({ "limit": 2, "include_hidden": true }))
            .await
            .unwrap();
        let windows = out["windows"].as_array().unwrap();
        assert_eq!(windows.len(), 2);
        assert_eq!(out["count"], windows.len());
    }

    #[tokio::test]
    async fn concurrent_calls_share_state_without_corruption() {
        let state = state_with(sample_windows());
        let mut handles = Vec::new();
        for _ in 0..8 {
            let state = state.clone();
            handles.push(tokio::spawn(async move {
                let out = call(&state, json!({})).await.unwrap();
                let windows = out["windows"].as_array().unwrap();
                assert_eq!(windows.len(), 3);
                assert_eq!(out["count"], windows.len());
                out
            }));
        }
        for handle in handles {
            handle.await.unwrap();
        }
    }

    #[tokio::test]
    async fn repeated_calls_are_consistent() {
        let state = state_with(sample_windows());
        for _ in 0..25 {
            let out = call(&state, json!({})).await.unwrap();
            let windows = out["windows"].as_array().unwrap();
            assert_eq!(windows.len(), 3);
            assert_eq!(out["count"], windows.len());
        }
    }

    #[tokio::test]
    async fn partial_metadata_failure_keeps_other_windows() {
        let mut windows = sample_windows();
        // The OpenCode window's process metadata is unresolvable.
        windows[1] = window(2, "OpenCode", 1002, None, true);
        let state = state_with(windows);
        let out = call(&state, json!({})).await.unwrap();
        let windows = out["windows"].as_array().unwrap();
        assert_eq!(windows.len(), 3);
        let broken = windows.iter().find(|w| w["hwnd"] == 2).unwrap();
        assert!(broken["process_name"].is_null());
        let named: Vec<&str> = windows
            .iter()
            .filter_map(|w| w["process_name"].as_str())
            .collect();
        assert_eq!(named.len(), 2);
        assert!(named.contains(&"chrome.exe"));
        assert!(named.contains(&"explorer.exe"));
    }
}
