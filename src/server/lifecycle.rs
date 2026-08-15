//! MCP session lifecycle: the initialize handshake.
//!
//! Per the MCP spec, a client must complete `initialize` before using
//! protocol features. WinKit tracks the handshake and rejects requests that
//! arrive before it with the `-32002` server-not-initialized error.

use serde_json::Value;
use std::sync::atomic::{AtomicBool, Ordering};

/// JSON-RPC code for "server has not been initialized".
pub const SERVER_NOT_INITIALIZED: i64 = -32002;

/// Session lifecycle state for one MCP connection.
#[derive(Debug, Default)]
pub struct Lifecycle {
    initialized: AtomicBool,
    client_name: std::sync::Mutex<Option<String>>,
    client_version: std::sync::Mutex<Option<String>>,
}

impl Lifecycle {
    pub fn new() -> Self {
        Self::default()
    }

    /// Record the client's identity from the `initialize` params.
    pub fn mark_initialized(&self, params: &Value) {
        self.initialized.store(true, Ordering::Relaxed);
        let info = params.get("clientInfo");
        *self.client_name.lock().unwrap() = info
            .and_then(|i| i.get("name"))
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());
        *self.client_version.lock().unwrap() = info
            .and_then(|i| i.get("version"))
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());
    }

    pub fn is_initialized(&self) -> bool {
        self.initialized.load(Ordering::Relaxed)
    }

    pub fn client_name(&self) -> Option<String> {
        self.client_name.lock().unwrap().clone()
    }

    pub fn client_version(&self) -> Option<String> {
        self.client_version.lock().unwrap().clone()
    }
}
