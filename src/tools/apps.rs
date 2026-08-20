//! Application tools: discovery, state, and Chrome info/tabs.

use crate::config::Config;
use crate::errors::WinkitError;
use crate::permissions::Capability;
use crate::server::AppState;
use crate::tools::chrome::{chrome_provider, chrome_timeout};
use crate::tools::{clamp_limit, optional_usize, required_string, wrap, ToolDefinition};
use serde_json::{json, Value};
use std::sync::Arc;

pub async fn list_applications_handler(
    state: Arc<AppState>,
    _args: Value,
) -> Result<Value, WinkitError> {
    let mut applications = Vec::new();
    for provider in state.applications.all() {
        match provider.info().await {
            Ok(info) => applications.push(json!({
                "id": info.id,
                "display_name": info.display_name,
                "version": info.version,
                "state": info.state,
                "capabilities": info.capabilities,
                "details": info.details,
            })),
            Err(e) => applications.push(json!({
                "id": provider.id(),
                "display_name": provider.display_name(),
                "state": "error",
                "error": e.message,
            })),
        }
    }
    let count = applications.len();
    let hint = if count == 0 {
        Some("No application adapters are registered — currently only Chrome is adapter-wrapped. For OS-level per-application memory/CPU (explorer, Chrome, Node, etc.) use system_health or system_diagnose.")
    } else {
        None
    };
    Ok(json!({
        "applications": applications,
        "count": count,
        "truncated": false,
        "providers": state.providers.all().iter().map(|p| p.id.clone()).collect::<Vec<_>>(),
        "note": "Application adapters are deep Chrome integrations (tabs, performance, memory). For all running Windows process groups by executable, use system_health / system_diagnose.",
        "hint": hint,
    }))
}

pub fn list_applications_definition() -> ToolDefinition {
    ToolDefinition {
        name: "list_applications",
        description:
            "List registered application adapters (currently Chrome) with availability and capabilities. This is NOT the Windows process-group list — for per-executable memory/CPU groups (explorer, Chrome, Spotify, Node, etc.) use system_health or system_diagnose.",
        input_schema: json!({ "type": "object", "properties": {}, "additionalProperties": false }),
        capability: Some(Capability::ApplicationDiscover),
        timeout_ms: None,
        handler: wrap(list_applications_handler),
    }
}

pub async fn get_application_handler(
    state: Arc<AppState>,
    args: Value,
) -> Result<Value, WinkitError> {
    let id = required_string(&args, "id")?;
    let provider = state.applications.get(&id).ok_or_else(|| {
        WinkitError::not_found(format!(
            "no application provider with id '{id}' (use list_applications to see registered adapters)"
        ))
    })?;
    let info = provider.info().await?;
    Ok(crate::tools::item_envelope("application", json!(info)))
}

pub fn get_application_definition() -> ToolDefinition {
    ToolDefinition {
        name: "get_application",
        description: "Detailed availability and capability information for one application adapter (e.g. 'chrome').",
        input_schema: json!({
            "type": "object",
            "properties": {
                "id": { "type": "string", "description": "Adapter id, e.g. 'chrome'." },
            },
            "required": ["id"],
            "additionalProperties": false,
        }),
        capability: Some(Capability::ApplicationDiscover),
        timeout_ms: None,
        handler: wrap(get_application_handler),
    }
}

pub async fn chrome_info_handler(state: Arc<AppState>, _args: Value) -> Result<Value, WinkitError> {
    let provider = chrome_provider(&state)?;
    let application = provider.info().await?;
    let browser = provider.browser_info().await?;
    Ok(json!({ "application": application, "browser": browser }))
}

pub fn chrome_info_definition(config: &Config) -> ToolDefinition {
    ToolDefinition {
        name: "chrome_info",
        description: "Chrome availability state, browser version, protocol version, tabs count, and Chrome processes.",
        input_schema: json!({ "type": "object", "properties": {}, "additionalProperties": false }),
        capability: Some(Capability::ApplicationDiscover),
        timeout_ms: chrome_timeout(config),
        handler: wrap(chrome_info_handler),
    }
}

pub async fn chrome_list_tabs_handler(
    state: Arc<AppState>,
    args: Value,
) -> Result<Value, WinkitError> {
    let provider = chrome_provider(&state)?;
    let max = state.config.chrome.max_tabs;
    let limit = clamp_limit(optional_usize(&args, "limit"), max);
    let mut tabs = provider.list_tabs().await?;
    tabs.truncate(limit);
    let count = tabs.len();
    Ok(crate::tools::list_envelope(
        "tabs",
        json!(tabs),
        count,
        limit,
    ))
}

pub fn chrome_list_tabs_definition(config: &Config) -> ToolDefinition {
    ToolDefinition {
        name: "chrome_list_tabs",
        description: "List open Chrome tabs with id, title, URL, and active state.",
        input_schema: json!({
            "type": "object",
            "properties": {
                "limit": { "type": "integer", "minimum": 1, "description": "Maximum results (defaults to the configured tab limit)." },
            },
            "additionalProperties": false,
        }),
        capability: Some(Capability::ApplicationTabsRead),
        timeout_ms: chrome_timeout(config),
        handler: wrap(chrome_list_tabs_handler),
    }
}

pub async fn chrome_get_tab_handler(
    state: Arc<AppState>,
    args: Value,
) -> Result<Value, WinkitError> {
    let tab_id = required_string(&args, "tab_id")?;
    let provider = chrome_provider(&state)?;
    let tab = provider.get_tab(&tab_id).await?;
    Ok(crate::tools::item_envelope("tab", json!(tab)))
}

pub fn chrome_get_tab_definition(config: &Config) -> ToolDefinition {
    ToolDefinition {
        name: "chrome_get_tab",
        description: "One Chrome tab by id (or exact URL).",
        input_schema: json!({
            "type": "object",
            "properties": {
                "tab_id": { "type": "string", "description": "Tab target id (from chrome_list_tabs) or exact URL." },
            },
            "required": ["tab_id"],
            "additionalProperties": false,
        }),
        capability: Some(Capability::ApplicationTabsRead),
        timeout_ms: chrome_timeout(config),
        handler: wrap(chrome_get_tab_handler),
    }
}

pub async fn chrome_get_active_tab_handler(
    state: Arc<AppState>,
    _args: Value,
) -> Result<Value, WinkitError> {
    let provider = chrome_provider(&state)?;
    let tab = provider.get_active_tab().await?;
    Ok(crate::tools::item_envelope("tab", json!(tab)))
}

pub fn chrome_get_active_tab_definition(config: &Config) -> ToolDefinition {
    ToolDefinition {
        name: "chrome_get_active_tab",
        description: "The currently active Chrome tab, determined by window-title correlation with the Windows foreground window.",
        input_schema: json!({ "type": "object", "properties": {}, "additionalProperties": false }),
        capability: Some(Capability::ApplicationTabsRead),
        timeout_ms: chrome_timeout(config),
        handler: wrap(chrome_get_active_tab_handler),
    }
}
