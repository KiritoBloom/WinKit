//! MCP stdio transport: newline-delimited JSON-RPC over stdin/stdout.
//!
//! WinKit is launched per MCP client session; the loop runs until stdin
//! closes or the client sends `exit`. All diagnostics go to stderr.

use crate::errors::WinkitError;
use crate::log_debug;
use crate::server::protocol::McpServer;
use crate::server::AppState;
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

/// Hard cap on a single inbound JSON-RPC frame (bytes). Frames larger than
/// this are rejected instead of buffered unboundedly.
const MAX_FRAME_BYTES: usize = 8 * 1024 * 1024;

/// Run the MCP server loop over stdio until the client disconnects.
pub async fn run(state: &Arc<AppState>) -> Result<(), WinkitError> {
    crate::log_info!("MCP transport ready — waiting for initialize");
    let server = McpServer::new(state.clone());
    let stdin = BufReader::new(tokio::io::stdin());
    let mut lines = stdin.lines();
    let mut stdout = tokio::io::stdout();

    while let Some(line) = lines.next_line().await.map_err(|e| {
        WinkitError::new(
            crate::errors::ErrorKind::ProtocolError,
            format!("failed to read from stdin: {e}"),
        )
    })? {
        if line.len() > MAX_FRAME_BYTES {
            log_debug!("rejecting oversized frame of {} bytes", line.len());
            let reply = serde_json::json!({
                "jsonrpc": "2.0",
                "id": serde_json::Value::Null,
                "error": { "code": -32700, "message": "frame exceeds the 8 MiB limit" },
            });
            write_frame(&mut stdout, &reply.to_string()).await?;
            continue;
        }

        let trimmed = line.trim_end();
        if trimmed.is_empty() {
            continue;
        }

        if let Some(reply) = server.handle_message(trimmed).await {
            write_frame(&mut stdout, &reply).await?;
        }
    }

    Ok(())
}

async fn write_frame(stdout: &mut tokio::io::Stdout, frame: &str) -> Result<(), WinkitError> {
    stdout.write_all(frame.as_bytes()).await.map_err(|e| {
        WinkitError::new(
            crate::errors::ErrorKind::ProtocolError,
            format!("stdout write failed: {e}"),
        )
    })?;
    stdout.write_all(b"\n").await.map_err(|e| {
        WinkitError::new(
            crate::errors::ErrorKind::ProtocolError,
            format!("stdout write failed: {e}"),
        )
    })?;
    stdout.flush().await.map_err(|e| {
        WinkitError::new(
            crate::errors::ErrorKind::ProtocolError,
            format!("stdout flush failed: {e}"),
        )
    })
}
