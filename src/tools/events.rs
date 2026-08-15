//! Event log tools: bounded, read-only event inspection (§19).
//!
//! Sensitive event data is never expanded: only the normalized fields in
//! `EventInfo` are returned, with the result count capped by configuration.

use crate::errors::WinkitError;
use crate::models::EventQuery;
use crate::permissions::Capability;
use crate::server::AppState;
use crate::tools::{
    clamp_limit, level_to_min, optional_string, optional_u32, optional_u64, optional_usize, wrap,
    ToolDefinition,
};
use serde_json::{json, Value};
use std::sync::Arc;

/// Build an `EventQuery` from tool arguments with defaults for log and level.
fn query_from_args(
    args: &Value,
    state: &AppState,
    default_log: &str,
    default_min_level: u32,
) -> Result<EventQuery, WinkitError> {
    let log = optional_string(args, "log").unwrap_or_else(|| default_log.to_string());
    let level = optional_string(args, "level")
        .map(|l| {
            level_to_min(&l).ok_or_else(|| {
                WinkitError::invalid_argument(format!(
                    "invalid level '{l}' (expected critical, error, warning, info, or verbose)"
                ))
            })
        })
        .transpose()?
        .unwrap_or(default_min_level);
    Ok(EventQuery {
        log,
        min_level: Some(level),
        since_minutes: optional_u64(args, "since_minutes"),
        provider: optional_string(args, "provider"),
        event_id: optional_u32(args, "event_id"),
        max_results: clamp_limit(
            optional_usize(args, "max_results"),
            state.config.limits.max_events,
        ),
    })
}

async fn run_event_query(
    state: Arc<AppState>,
    args: Value,
    default_log: &str,
    default_min_level: u32,
) -> Result<Value, WinkitError> {
    let query = query_from_args(&args, &state, default_log, default_min_level)?;
    let events = state.windows.get_recent_events(&query)?;
    let count = events.len();
    Ok(json!({
        "log": query.log,
        "events": events,
        "count": count,
        "truncated": count == query.max_results,
    }))
}

pub async fn get_recent_events_handler(
    state: Arc<AppState>,
    args: Value,
) -> Result<Value, WinkitError> {
    run_event_query(state, args, "Application", 4).await
}

pub async fn get_application_errors_handler(
    state: Arc<AppState>,
    args: Value,
) -> Result<Value, WinkitError> {
    run_event_query(state, args, "Application", 2).await
}

pub async fn get_system_errors_handler(
    state: Arc<AppState>,
    args: Value,
) -> Result<Value, WinkitError> {
    run_event_query(state, args, "System", 2).await
}

fn event_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "log": { "type": "string", "description": "Channel/log name, e.g. 'Application' or 'System'." },
            "level": { "type": "string", "enum": ["critical", "error", "warning", "info", "verbose"], "description": "Minimum severity to include." },
            "provider": { "type": "string", "description": "Restrict to one provider/source name." },
            "event_id": { "type": "integer", "description": "Restrict to one event ID." },
            "since_minutes": { "type": "integer", "minimum": 1, "description": "Only events newer than this many minutes." },
            "max_results": { "type": "integer", "minimum": 1, "description": "Maximum results (defaults to the configured limit)." },
        },
        "additionalProperties": false,
    })
}

pub fn get_recent_events_definition() -> ToolDefinition {
    ToolDefinition {
        name: "get_recent_events",
        description: "Recent Windows event log entries, normalized and bounded. Defaults to the Application log at information level.",
        input_schema: event_schema(),
        capability: Some(Capability::EventRead),
        timeout_ms: None,
        handler: wrap(get_recent_events_handler),
    }
}

pub fn get_application_errors_definition() -> ToolDefinition {
    ToolDefinition {
        name: "get_application_errors",
        description: "Recent errors from the Application event log.",
        input_schema: event_schema(),
        capability: Some(Capability::EventRead),
        timeout_ms: None,
        handler: wrap(get_application_errors_handler),
    }
}

pub fn get_system_errors_definition() -> ToolDefinition {
    ToolDefinition {
        name: "get_system_errors",
        description: "Recent errors from the System event log.",
        input_schema: event_schema(),
        capability: Some(Capability::EventRead),
        timeout_ms: None,
        handler: wrap(get_system_errors_handler),
    }
}
