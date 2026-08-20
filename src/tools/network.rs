//! Network tools: listening ports, port ownership, interfaces, and connections.

use crate::errors::WinkitError;
use crate::permissions::Capability;
use crate::server::AppState;
use crate::tools::{clamp_limit, optional_usize, wrap, ToolDefinition};
use serde_json::{json, Value};
use std::sync::Arc;

pub async fn list_listening_ports_handler(
    state: Arc<AppState>,
    args: Value,
) -> Result<Value, WinkitError> {
    let max = state.config.limits.max_network_results;
    let limit = clamp_limit(optional_usize(&args, "limit"), max);
    let ports = state.windows.list_listening_ports(limit)?;
    let count = ports.len();
    Ok(crate::tools::list_envelope(
        "ports",
        json!(ports),
        count,
        limit,
    ))
}

pub fn list_listening_ports_definition() -> ToolDefinition {
    ToolDefinition {
        name: "list_listening_ports",
        description:
            "List TCP/UDP ports currently listening, with the owning process when resolvable.",
        input_schema: json!({
            "type": "object",
            "properties": {
                "limit": { "type": "integer", "minimum": 1, "description": "Maximum results (defaults to the configured limit)." },
            },
            "additionalProperties": false,
        }),
        capability: Some(Capability::NetworkRead),
        timeout_ms: None,
        handler: wrap(list_listening_ports_handler),
    }
}

pub async fn find_process_on_port_handler(
    state: Arc<AppState>,
    args: Value,
) -> Result<Value, WinkitError> {
    let port = crate::tools::parse_port(&args, "port")?;
    match state.windows.find_process_on_port(port)? {
        Some(info) => Ok(json!({ "port": info, "found": true, "port_number": port })),
        None => Ok(json!({
            "port": port,
            "port_number": port,
            "found": false,
            "process": null,
            "message": format!("no process is listening on port {port}")
        })),
    }
}

pub fn find_process_on_port_definition() -> ToolDefinition {
    ToolDefinition {
        name: "find_process_on_port",
        description: "Find which process is listening on a TCP port ('What's using port 3000?').",
        input_schema: json!({
            "type": "object",
            "properties": {
                "port": { "type": "integer", "minimum": 1, "maximum": 65535 },
            },
            "required": ["port"],
            "additionalProperties": false,
        }),
        capability: Some(Capability::NetworkRead),
        timeout_ms: None,
        handler: wrap(find_process_on_port_handler),
    }
}

pub async fn list_network_interfaces_handler(
    state: Arc<AppState>,
    _args: Value,
) -> Result<Value, WinkitError> {
    let interfaces = state.windows.list_network_interfaces()?;
    let count = interfaces.len();
    Ok(crate::tools::list_envelope(
        "interfaces",
        json!(interfaces),
        count,
        count,
    ))
}

pub fn list_network_interfaces_definition() -> ToolDefinition {
    ToolDefinition {
        name: "list_network_interfaces",
        description: "List network interfaces with IPv4 addresses, masks, MAC, and gateway.",
        input_schema: json!({ "type": "object", "properties": {}, "additionalProperties": false }),
        capability: Some(Capability::NetworkRead),
        timeout_ms: None,
        handler: wrap(list_network_interfaces_handler),
    }
}

pub async fn list_connections_handler(
    state: Arc<AppState>,
    args: Value,
) -> Result<Value, WinkitError> {
    let max = state.config.limits.max_network_results;
    let limit = clamp_limit(optional_usize(&args, "limit"), max);
    let connections = state.windows.list_connections(limit)?;
    let count = connections.len();
    Ok(crate::tools::list_envelope(
        "connections",
        json!(connections),
        count,
        limit,
    ))
}

pub fn list_connections_definition() -> ToolDefinition {
    ToolDefinition {
        name: "list_connections",
        description: "List active TCP connections (IPv4) with owning process when resolvable.",
        input_schema: json!({
            "type": "object",
            "properties": {
                "limit": { "type": "integer", "minimum": 1, "description": "Maximum results (defaults to the configured limit)." },
            },
            "additionalProperties": false,
        }),
        capability: Some(Capability::NetworkRead),
        timeout_ms: None,
        handler: wrap(list_connections_handler),
    }
}
