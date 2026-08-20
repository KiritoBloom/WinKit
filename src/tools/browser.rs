//! Managed-browser tools: owned Chrome sessions WinKit creates and inspects.
//!
//! Lifecycle tools are action-capability gated and feature-gated by
//! `[chrome.managed] enabled`. Inspection tools are read-only and
//! permission-gated. No tool accepts arbitrary paths, flags, CDP methods,
//! or JavaScript.

use crate::errors::WinkitError;
use crate::permissions::Capability;
use crate::server::AppState;
use crate::tools::{optional_bool, optional_u64, required_u64, wrap, ToolDefinition};
use crate::utils::url::{validate_url, UrlPolicy};
use serde_json::{json, Value};
use std::sync::Arc;

/// URL policy for managed navigation: the managed flag decides external
/// access, dev hosts come from `[web]`, and local TLS follows `[web]`.
fn managed_url_policy(state: &AppState) -> UrlPolicy {
    UrlPolicy {
        allow_external: state.config.chrome.managed.allow_external_urls,
        dev_hosts: state.config.web.dev_hosts.clone(),
        local_tls_allowed: state.config.web.local_tls_allowed,
    }
}

// chrome_start_managed_session

pub async fn chrome_start_managed_session_handler(
    state: Arc<AppState>,
    args: Value,
) -> Result<Value, WinkitError> {
    let url = crate::tools::required_string(&args, "url")?;
    let headless =
        optional_bool(&args, "headless").unwrap_or(state.config.chrome.managed.default_headless);
    let reuse_existing = optional_bool(&args, "reuse_existing").unwrap_or(true);
    let wait_ms = optional_u64(&args, "wait_for_ready_ms");

    let validated = validate_url(&url, &managed_url_policy(&state))?;

    let manager = state.managed.clone();
    if reuse_existing {
        if let Some(existing) = manager.find_ready_for(&validated.display()).await {
            return Ok(existing.summary_value());
        }
    }
    let session = manager.start(Some(&validated), headless, wait_ms).await?;
    Ok(session.summary_value())
}

pub fn chrome_start_managed_session_definition() -> ToolDefinition {
    ToolDefinition {
        name: "chrome_start_managed_session",
        description: "Start an isolated WinKit-owned Chrome session pointed at a local URL. Use it when a local page needs browser-level evidence (runtime errors, failed requests, blank page) without the developer manually starting Chrome with a debugging flag. By default (headless=false) a real visible Chrome window opens on the desktop; pass headless=true for a non-visible automation/CI session that opens no window by design. Creates a dedicated profile under the managed root, binds DevTools to loopback, and returns an opaque session_id plus the selected mode (headless, window_mode, launch_mode) for the other managed tools. Never attaches to the developer's normal Chrome profile; never accepts executable paths, profile paths, flags, or JavaScript. Changes state only for resources WinKit creates. Latency: up to startup_timeout_ms (default 10 s). Requires [chrome.managed] enabled = true and the application.browser.launch permission. Next: chrome_get_page_summary, chrome_capture_screenshot, chrome_stop_managed_session.",
        input_schema: json!({
            "type": "object",
            "properties": {
                "url": { "type": "string", "description": "Local http(s) URL to open (localhost/127.0.0.1/[::1], or a host allowed by the managed external-URL policy)." },
                "headless": { "type": "boolean", "optional": true, "default": false, "description": "Launch a visible headed Chrome window when false. Launch a non-visible headless session when true." },
                "reuse_existing": { "type": "boolean", "description": "Reuse an existing ready session for the same URL (default true)." },
                "wait_for_ready_ms": { "type": "integer", "minimum": 250, "description": "Deadline for endpoint readiness, clamped to startup_timeout_ms." }
            },
            "required": ["url"],
            "additionalProperties": false
        }),
        capability: None,
        timeout_ms: Some(90_000),
        handler: wrap(chrome_start_managed_session_handler),
    }
}

// chrome_list_managed_sessions

pub async fn chrome_list_managed_sessions_handler(
    state: Arc<AppState>,
    _args: Value,
) -> Result<Value, WinkitError> {
    let sessions = state.managed.list().await;
    Ok(json!({
        "enabled": state.managed.enabled(),
        "sessions": sessions,
        "count": sessions.len(),
        "truncated": false,
    }))
}

pub fn chrome_list_managed_sessions_definition() -> ToolDefinition {
    ToolDefinition {
        name: "chrome_list_managed_sessions",
        description: "List the Chrome sessions WinKit itself started, with state, port, and redacted URL for each. Call it to find a session_id or check whether a managed browser is still running. Only WinKit-owned sessions are ever reported; normal Chrome profiles are never attached or listed. Read-only, typically <10 ms. Next: chrome_get_page_summary on a ready session, or chrome_stop_managed_session to close one.",
        input_schema: json!({
            "type": "object",
            "properties": {},
            "additionalProperties": false
        }),
        capability: Some(Capability::ApplicationTabsRead),
        timeout_ms: None,
        handler: wrap(chrome_list_managed_sessions_handler),
    }
}

// chrome_navigate_managed_session

pub async fn chrome_navigate_managed_session_handler(
    state: Arc<AppState>,
    args: Value,
) -> Result<Value, WinkitError> {
    let session_id = crate::tools::required_string(&args, "session_id")?;
    let url = crate::tools::required_string(&args, "url")?;
    let validated = validate_url(&url, &managed_url_policy(&state))?;
    let session = state.managed.get(&session_id).await?;
    state.managed.navigate(&session, &validated).await
}

pub fn chrome_navigate_managed_session_definition() -> ToolDefinition {
    ToolDefinition {
        name: "chrome_navigate_managed_session",
        description: "Navigate a WinKit-owned managed tab to another validated local URL. Use it to inspect a different route of the same app or a second local service without starting a new browser. The URL goes through the same loopback/development-host validation as every web tool; external hosts require the managed external-URL policy. Never executes JavaScript and never reads cookies, headers, or bodies. Changes only the owned tab. Latency: <1 s typical. Next: chrome_get_page_summary for the new page.",
        input_schema: json!({
            "type": "object",
            "properties": {
                "session_id": { "type": "string", "description": "Opaque id from chrome_start_managed_session." },
                "url": { "type": "string", "description": "Local http(s) URL to navigate to." }
            },
            "required": ["session_id", "url"],
            "additionalProperties": false
        }),
        capability: None,
        timeout_ms: Some(30_000),
        handler: wrap(chrome_navigate_managed_session_handler),
    }
}

// chrome_stop_managed_session

pub async fn chrome_stop_managed_session_handler(
    state: Arc<AppState>,
    args: Value,
) -> Result<Value, WinkitError> {
    let session_id = crate::tools::required_string(&args, "session_id")?;
    state.managed.stop(&session_id).await
}

pub fn chrome_stop_managed_session_definition() -> ToolDefinition {
    ToolDefinition {
        name: "chrome_stop_managed_session",
        description: "Gracefully close a WinKit-owned managed Chrome session and remove its profile. Call it when browser evidence is collected; never leave a managed browser running unnecessarily. Only ever closes sessions WinKit started; arbitrary Chrome processes and the developer's normal profile are never touched. Changes state only for owned resources. Latency: up to ~5 s while the browser exits. Next: chrome_list_managed_sessions to confirm cleanup.",
        input_schema: json!({
            "type": "object",
            "properties": {
                "session_id": { "type": "string", "description": "Opaque id from chrome_start_managed_session." }
            },
            "required": ["session_id"],
            "additionalProperties": false
        }),
        capability: None,
        timeout_ms: Some(30_000),
        handler: wrap(chrome_stop_managed_session_handler),
    }
}

// chrome_get_page_summary

pub async fn chrome_get_page_summary_handler(
    state: Arc<AppState>,
    args: Value,
) -> Result<Value, WinkitError> {
    let session_id = crate::tools::required_string(&args, "session_id")?;
    let observe_ms = optional_u64(&args, "observe_ms");
    let session = state.managed.get(&session_id).await?;
    state.managed.page_summary(&session, observe_ms).await
}

pub fn chrome_get_page_summary_definition() -> ToolDefinition {
    ToolDefinition {
        name: "chrome_get_page_summary",
        description: "Bounded summary of the page in a WinKit-owned managed tab: title, redacted URL, headings, landmarks, form labels without values, visible-text stats, and a short observation of runtime errors and network failures. Call it to learn why a local page is blank or broken. Never reads form values, cookies, headers, or request bodies; text is truncated to the configured cap. Read-only; latency = summary + up to observe_ms (default 3 s). Next: chrome_capture_screenshot for a visual check, or chrome_stop_managed_session when finished.",
        input_schema: json!({
            "type": "object",
            "properties": {
                "session_id": { "type": "string", "description": "Opaque id from chrome_start_managed_session." },
                "observe_ms": { "type": "integer", "minimum": 0, "maximum": 30000, "description": "How long to watch for runtime errors and failed requests (default [chrome] observation_window_ms)." }
            },
            "required": ["session_id"],
            "additionalProperties": false
        }),
        capability: Some(Capability::ApplicationRuntimeRead),
        timeout_ms: Some(40_000),
        handler: wrap(chrome_get_page_summary_handler),
    }
}

// chrome_capture_screenshot

pub async fn chrome_capture_screenshot_handler(
    state: Arc<AppState>,
    args: Value,
) -> Result<Value, WinkitError> {
    let session_id = crate::tools::required_string(&args, "session_id")?;
    let session = state.managed.get(&session_id).await?;
    state.managed.capture_screenshot(&session).await
}

pub fn chrome_capture_screenshot_definition() -> ToolDefinition {
    ToolDefinition {
        name: "chrome_capture_screenshot",
        description: "Capture a PNG screenshot of the page in a WinKit-owned managed tab, returned as base64 with dimension and byte caps applied. Use it to visually confirm a blank page, layout breakage, or error screen. Only authorized managed tabs are captured; the image is bounded by [chrome.managed] max_screenshot_dimension and max_screenshot_bytes. Read-only; latency <2 s typical. Next: chrome_get_page_summary for the underlying runtime/network evidence, or chrome_stop_managed_session when finished.",
        input_schema: json!({
            "type": "object",
            "properties": {
                "session_id": { "type": "string", "description": "Opaque id from chrome_start_managed_session." }
            },
            "required": ["session_id"],
            "additionalProperties": false
        }),
        capability: Some(Capability::ApplicationDiagnosticsRead),
        timeout_ms: Some(30_000),
        handler: wrap(chrome_capture_screenshot_handler),
    }
}

// chrome_approve_managed_action

pub async fn chrome_approve_managed_action_handler(
    state: Arc<AppState>,
    args: Value,
) -> Result<Value, WinkitError> {
    let request_id = required_u64(&args, "request_id")?;
    state.permissions.grant_approval(request_id)?;
    let pending = state.permissions.pending_approvals();
    Ok(json!({
        "approved": request_id,
        "pending_approvals": pending.len(),
    }))
}

pub fn chrome_approve_managed_action_definition() -> ToolDefinition {
    ToolDefinition {
        name: "chrome_approve_managed_action",
        description: "Explicitly approve one pending managed-browser action in approval permission mode. When a lifecycle tool (chrome_start_managed_session, chrome_navigate_managed_session, chrome_stop_managed_session) returns approval_required with a request_id, call this with that id, then retry the action. The grant is per-request, not a standing permission. Read-only with respect to the machine; it only changes WinKit's own approval state. Latency: <10 ms. Next: retry the tool that requested approval.",
        input_schema: json!({
            "type": "object",
            "properties": {
                "request_id": { "type": "integer", "minimum": 1, "description": "request_id from the approval_required error." }
            },
            "required": ["request_id"],
            "additionalProperties": false
        }),
        capability: Some(Capability::ApplicationDiscover),
        timeout_ms: None,
        handler: wrap(chrome_approve_managed_action_handler),
    }
}
