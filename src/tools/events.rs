//! Event log tools: bounded, read-only inspection.
//!
//! Sensitive event data is never expanded: only the normalized fields in
//! `EventInfo` are returned, with the result count capped by configuration.

use crate::errors::WinkitError;
use crate::models::EventQuery;
use crate::permissions::Capability;
use crate::server::AppState;
use crate::tools::{
    clamp_limit, level_to_min, optional_bool, optional_non_empty_string, optional_u32,
    optional_u64, optional_usize, wrap, ToolDefinition,
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
    let log = optional_non_empty_string(args, "log").unwrap_or_else(|| default_log.to_string());
    let level = optional_non_empty_string(args, "level")
        .map(|trimmed| {
            level_to_min(&trimmed).ok_or_else(|| {
                WinkitError::invalid_argument(format!(
                    "invalid level '{trimmed}' (expected critical, error, warning, info, or verbose)"
                ))
            })
        })
        .transpose()?
        .unwrap_or(default_min_level);
    let provider = optional_non_empty_string(args, "provider");
    Ok(EventQuery {
        log,
        min_level: Some(level),
        since_minutes: optional_u64(args, "since_minutes").map(|v| v.clamp(1, 129_600)),
        provider,
        event_id: optional_u32(args, "event_id"),
        max_results: clamp_limit(
            optional_usize(args, "max_results"),
            state.config.limits.max_events,
        ),
    })
}

/// Run a query and project the response.
///
/// `skip_null_default` is the default for `skip_null_messages`: error tools
/// drop events whose provider publishes no message template by default,
/// because a flood of `message: null` entries (e.g. 148 identical Spotify
/// Event 100 rows) buries the real crashes an agent is looking for. Targeted
/// queries (`provider` or `event_id` given) keep every match.
async fn run_event_query(
    state: Arc<AppState>,
    args: Value,
    default_log: &str,
    default_min_level: u32,
    skip_null_default: bool,
) -> Result<Value, WinkitError> {
    let query = query_from_args(&args, &state, default_log, default_min_level)?;
    let mut events = state.windows.get_recent_events(&query)?;
    let skip_nulls = optional_bool(&args, "skip_null_messages")
        .unwrap_or(skip_null_default && query.provider.is_none() && query.event_id.is_none());
    if skip_nulls {
        let dropped = events.iter().filter(|e| e.message.is_none()).count();
        events.retain(|e| e.message.is_some());
        let count = events.len();
        return Ok(json!({
            "log": query.log,
            "events": events,
            "count": count,
            "truncated": count == query.max_results,
            "skipped_null_messages": dropped,
        }));
    }
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
    run_event_query(state, args, "Application", 4, false).await
}

pub async fn get_application_errors_handler(
    state: Arc<AppState>,
    args: Value,
) -> Result<Value, WinkitError> {
    run_event_query(state, args, "Application", 2, true).await
}

pub async fn get_system_errors_handler(
    state: Arc<AppState>,
    args: Value,
) -> Result<Value, WinkitError> {
    run_event_query(state, args, "System", 2, true).await
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
            "skip_null_messages": { "type": "boolean", "description": "Drop events whose provider publishes no message text. Defaults to true for get_application_errors/get_system_errors unless provider or event_id filters are given; false for get_recent_events." },
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
        description: "Recent errors from the Application event log. By default events whose provider publishes no message text are skipped (they are noise — e.g. repeated null-message entries that bury real crashes); pass skip_null_messages=false or filter by provider/event_id to see them.",
        input_schema: event_schema(),
        capability: Some(Capability::EventRead),
        timeout_ms: None,
        handler: wrap(get_application_errors_handler),
    }
}

pub fn get_system_errors_definition() -> ToolDefinition {
    ToolDefinition {
        name: "get_system_errors",
        description: "Recent errors from the System event log. By default events whose provider publishes no message text are skipped (noise); pass skip_null_messages=false or filter by provider/event_id to see them.",
        input_schema: event_schema(),
        capability: Some(Capability::EventRead),
        timeout_ms: None,
        handler: wrap(get_system_errors_handler),
    }
}
