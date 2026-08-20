//! Chrome tab inspection: performance, memory, network, runtime, and diagnostics.

use crate::config::Config;
use crate::errors::WinkitError;
use crate::permissions::Capability;
use crate::server::AppState;
use crate::tools::chrome::{chrome_provider, chrome_timeout, tab_id_schema};
use crate::tools::{optional_u64, required_string, wrap, ToolDefinition};
use serde_json::{json, Value};
use std::sync::Arc;

pub async fn chrome_get_tab_performance_handler(
    state: Arc<AppState>,
    args: Value,
) -> Result<Value, WinkitError> {
    let tab_id = required_string(&args, "tab_id")?;
    let provider = chrome_provider(&state)?;
    let performance = provider.tab_performance(&tab_id).await?;
    Ok(json!({ "tab_id": tab_id, "performance": performance }))
}

pub fn chrome_get_tab_performance_definition(config: &Config) -> ToolDefinition {
    ToolDefinition {
        name: "chrome_get_tab_performance",
        description: "Performance metrics for one Chrome tab (CPU-ish timing metrics, long tasks, script duration, and deltas between two samples).",
        input_schema: tab_id_schema(),
        capability: Some(Capability::ApplicationPerformanceRead),
        timeout_ms: chrome_timeout(config),
        handler: wrap(chrome_get_tab_performance_handler),
    }
}

pub async fn chrome_get_tab_memory_handler(
    state: Arc<AppState>,
    args: Value,
) -> Result<Value, WinkitError> {
    let tab_id = required_string(&args, "tab_id")?;
    let provider = chrome_provider(&state)?;
    let memory = provider.tab_memory(&tab_id).await?;
    Ok(json!({ "tab_id": tab_id, "memory": memory }))
}

pub fn chrome_get_tab_memory_definition(config: &Config) -> ToolDefinition {
    ToolDefinition {
        name: "chrome_get_tab_memory",
        description: "Memory picture of one Chrome tab: JS heap, DOM counters, and heap growth between two samples.",
        input_schema: tab_id_schema(),
        capability: Some(Capability::ApplicationMemoryRead),
        timeout_ms: chrome_timeout(config),
        handler: wrap(chrome_get_tab_memory_handler),
    }
}

pub async fn chrome_get_tab_network_handler(
    state: Arc<AppState>,
    args: Value,
) -> Result<Value, WinkitError> {
    let tab_id = required_string(&args, "tab_id")?;
    let provider = chrome_provider(&state)?;
    let network = provider.tab_network(&tab_id).await?;
    Ok(json!({ "tab_id": tab_id, "network": network }))
}

pub fn chrome_get_tab_network_definition(config: &Config) -> ToolDefinition {
    ToolDefinition {
        name: "chrome_get_tab_network",
        description: "Network activity for one Chrome tab during the observation window: request counts, failures, latency, slowest requests. Headers, cookies, and bodies are never captured.",
        input_schema: tab_id_schema(),
        capability: Some(Capability::ApplicationNetworkRead),
        timeout_ms: chrome_timeout(config),
        handler: wrap(chrome_get_tab_network_handler),
    }
}

pub async fn chrome_get_tab_runtime_handler(
    state: Arc<AppState>,
    args: Value,
) -> Result<Value, WinkitError> {
    let tab_id = required_string(&args, "tab_id")?;
    let provider = chrome_provider(&state)?;
    let runtime = provider.tab_runtime(&tab_id).await?;
    Ok(json!({ "tab_id": tab_id, "runtime": runtime }))
}

pub fn chrome_get_tab_runtime_definition(config: &Config) -> ToolDefinition {
    ToolDefinition {
        name: "chrome_get_tab_runtime",
        description: "Console errors, warnings, exceptions, and page state for one Chrome tab during the observation window. Output is truncated and never contains secrets.",
        input_schema: tab_id_schema(),
        capability: Some(Capability::ApplicationRuntimeRead),
        timeout_ms: chrome_timeout(config),
        handler: wrap(chrome_get_tab_runtime_handler),
    }
}

pub async fn chrome_diagnose_tab_handler(
    state: Arc<AppState>,
    args: Value,
) -> Result<Value, WinkitError> {
    let tab_id = required_string(&args, "tab_id")?;
    let provider = chrome_provider(&state)?;
    let windows: &dyn crate::providers::windows::WindowsBackend = state.windows.as_ref();
    let diagnostics = provider.tab_diagnostics(&tab_id, windows).await?;
    Ok(json!({
        "tab": diagnostics.tab,
        "resource_usage": diagnostics.resource_usage,
        "performance": diagnostics.performance,
        "memory": diagnostics.memory,
        "network": diagnostics.network,
        "runtime": diagnostics.runtime,
        "report": diagnostics.report,
    }))
}

pub fn chrome_diagnose_tab_definition(config: &Config) -> ToolDefinition {
    ToolDefinition {
        name: "chrome_diagnose_tab",
        description: "Cross-layer diagnostics for one Chrome tab: tab metadata, Windows-side Chrome resource usage, performance, memory, network, runtime, and deterministic evidence-based signals with possible causes. Signals are heuristics, not root-cause claims.",
        input_schema: tab_id_schema(),
        capability: Some(Capability::ApplicationDiagnosticsRead),
        timeout_ms: chrome_timeout(config),
        handler: wrap(chrome_diagnose_tab_handler),
    }
}

pub async fn chrome_tab_trend_handler(
    state: Arc<AppState>,
    args: Value,
) -> Result<Value, WinkitError> {
    let tab_id = required_string(&args, "tab_id")?;
    let observe_ms = optional_u64(&args, "observe_ms").unwrap_or(10_000);
    let observe_ms = observe_ms.clamp(2_000, state.config.chrome.trend_max_ms);
    let provider = chrome_provider(&state)?;
    let trend = provider.tab_trend(&tab_id, observe_ms).await?;
    Ok(json!({ "tab_id": tab_id, "observe_ms": observe_ms, "trend": trend }))
}

pub fn chrome_tab_trend_definition(config: &Config) -> ToolDefinition {
    ToolDefinition {
        name: "chrome_tab_trend",
        description: "Time-series view of one Chrome tab: JS heap and script/long-task activity sampled every few seconds over an observation window (default 10 s), reduced to what changed — growth, rate, sustained growth — plus a deterministic evidence-based report. Use it to answer 'what changed in this tab over the last N seconds'.",
        input_schema: json!({
            "type": "object",
            "properties": {
                "tab_id": { "type": "string", "description": "Tab target id (from chrome_list_tabs) or exact URL." },
                "observe_ms": { "type": "integer", "description": "Observation window in milliseconds (default 10000, clamped between 2000 and the configured maximum)." },
            },
            "required": ["tab_id"],
            "additionalProperties": false,
        }),
        capability: Some(Capability::ApplicationDiagnosticsRead),
        timeout_ms: Some(config.chrome.operation_timeout_ms + config.chrome.trend_max_ms + 5_000),
        handler: wrap(chrome_tab_trend_handler),
    }
}
