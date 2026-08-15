//! JSON-RPC 2.0 + MCP protocol handling.
//!
//! One [`McpServer`] serves a single stdio session. It implements the
//! minimal MCP surface WinKit needs: `initialize`, `ping`, `tools/list`,
//! and `tools/call`, plus the standard `shutdown`/`exit` notifications.
//! Unknown methods and malformed frames produce spec-compliant errors.

use crate::errors::WinkitError;
use crate::log_debug;
use crate::server::lifecycle::{Lifecycle, SERVER_NOT_INITIALIZED};
use crate::server::registry;
use crate::server::AppState;
use serde_json::{json, Value};
use std::sync::Arc;

/// The MCP protocol version WinKit speaks.
pub const PROTOCOL_VERSION: &str = "2024-11-05";

/// One MCP session.
pub struct McpServer {
    state: Arc<AppState>,
    lifecycle: Lifecycle,
}

impl McpServer {
    pub fn new(state: Arc<AppState>) -> Self {
        Self {
            state,
            lifecycle: Lifecycle::new(),
        }
    }

    /// Handle one inbound JSON-RPC frame. Returns the reply frame, or `None`
    /// for notifications and for `exit` (which ends the session).
    pub async fn handle_message(&self, frame: &str) -> Option<String> {
        let msg: Value = match serde_json::from_str(frame) {
            Ok(v) => v,
            Err(e) => {
                return Some(
                    json!({
                        "jsonrpc": "2.0",
                        "id": Value::Null,
                        "error": { "code": -32700, "message": format!("parse error: {e}") },
                    })
                    .to_string(),
                );
            }
        };

        let id = msg.get("id").cloned();
        let is_notification = id.is_none();
        let method = msg
            .get("method")
            .and_then(|m| m.as_str())
            .unwrap_or_default()
            .to_string();
        let params = msg.get("params").cloned().unwrap_or(Value::Null);

        let result = match method.as_str() {
            "initialize" => Ok(self.initialize(&params)),
            "notifications/initialized" => {
                // Nothing to do beyond acknowledging the handshake.
                self.lifecycle.mark_initialized(&params);
                return None;
            }
            "ping" => {
                if !self.lifecycle.is_initialized() {
                    return Some(self.error(
                        SERVER_NOT_INITIALIZED,
                        "server not initialized",
                        json!({}),
                        id,
                    ));
                }
                Ok(json!({}))
            }
            "tools/list" => {
                if !self.lifecycle.is_initialized() {
                    return Some(self.error(
                        SERVER_NOT_INITIALIZED,
                        "server not initialized",
                        json!({}),
                        id,
                    ));
                }
                Ok(self.tools_list())
            }
            "tools/call" => {
                if !self.lifecycle.is_initialized() {
                    return Some(self.error(
                        SERVER_NOT_INITIALIZED,
                        "server not initialized",
                        json!({}),
                        id,
                    ));
                }
                self.tools_call(&params).await
            }
            "shutdown" => Ok(json!({})),
            "exit" => return None, // Client asked the session to end.
            "" => {
                return Some(self.error(
                    -32600,
                    "invalid request: missing 'method'",
                    json!({}),
                    id,
                ));
            }
            other => {
                return Some(self.error(
                    -32601,
                    &format!("method not found: {other}"),
                    json!({}),
                    id,
                ));
            }
        };

        let reply = match result {
            Ok(value) => {
                json!({ "jsonrpc": "2.0", "id": id.unwrap_or(Value::Null), "result": value })
                    .to_string()
            }
            Err(e) => {
                if !is_notification {
                    log_debug!("request '{}' failed: {}", method, e.message);
                }
                let (code, data) = registry::map_protocol_error(&e);
                self.error(code, &e.message, data, id)
            }
        };
        Some(reply)
    }

    fn initialize(&self, params: &Value) -> Value {
        self.lifecycle.mark_initialized(params);
        let client = params.get("clientInfo");
        log_debug!(
            "initialize from '{}' (version {})",
            client
                .and_then(|c| c.get("name"))
                .and_then(|v| v.as_str())
                .unwrap_or("unknown"),
            client
                .and_then(|c| c.get("version"))
                .and_then(|v| v.as_str())
                .unwrap_or("unknown")
        );
        json!({
            "protocolVersion": PROTOCOL_VERSION,
            "capabilities": { "tools": { "listChanged": false } },
            "serverInfo": { "name": "winkit", "version": env!("CARGO_PKG_VERSION") },
        })
    }

    fn tools_list(&self) -> Value {
        let tools: Vec<Value> = self
            .state
            .tools
            .schemas()
            .into_iter()
            .map(|(name, description, input_schema)| {
                json!({
                    "name": name,
                    "description": description,
                    "inputSchema": input_schema,
                })
            })
            .collect();
        json!({ "tools": tools })
    }

    async fn tools_call(&self, params: &Value) -> Result<Value, WinkitError> {
        let name = params
            .get("name")
            .and_then(|v| v.as_str())
            .ok_or_else(|| WinkitError::invalid_argument("tools/call requires a 'name' string"))?;
        let args = params.get("arguments").cloned().unwrap_or(json!({}));
        if !args.is_object() {
            return Err(WinkitError::invalid_argument(
                "tools/call 'arguments' must be an object",
            ));
        }
        let result = registry::call_tool(&self.state, name, args).await?;
        let text = serde_json::to_string(&result)?;
        Ok(json!({
            "content": [ { "type": "text", "text": text } ],
            "isError": false,
        }))
    }

    fn error(
        &self,
        code: i64,
        message: &str,
        data: serde_json::Value,
        id: Option<Value>,
    ) -> String {
        json!({
            "jsonrpc": "2.0",
            "id": id.unwrap_or(Value::Null),
            "error": { "code": code, "message": message, "data": data },
        })
        .to_string()
    }
}
