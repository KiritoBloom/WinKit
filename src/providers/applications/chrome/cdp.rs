//! Minimal Chrome DevTools Protocol (CDP) client over a single browser
//! WebSocket.
//!
//! Design:
//! - One WebSocket to the browser endpoint (`ws://127.0.0.1:PORT/...`).
//! - Page targets are attached with `Target.attachToTarget(flatten: true)`,
//!   producing a `sessionId` that multiplexes commands/events over the one
//!   socket.
//! - Responses are correlated by JSON-RPC id; events are broadcast to
//!   subscribers tagged with their `sessionId`.
//!
//! Security: this client never sends or reads cookies, credentials, or
//! headers. Network events only carry sanitized URLs, status codes, and
//! timings (see `session.rs`).

use crate::errors::{ErrorKind, WinkitError};
use futures_util::{SinkExt, StreamExt};
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::net::TcpStream;
use tokio::sync::{broadcast, oneshot, Mutex};
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::{connect_async, MaybeTlsStream, WebSocketStream};

/// Default timeout for a single CDP command round-trip.
pub const CDP_CALL_TIMEOUT: Duration = Duration::from_secs(10);

/// Is a WebSocket URL bound to a loopback address? CDP must never be dialed
/// remotely. Shared by the connection guard and the managed-session
/// lifecycle, which verifies an endpoint before trusting it.
pub(crate) fn ws_is_loopback(ws_url: &str) -> bool {
    ws_url
        .strip_prefix("ws://")
        .and_then(|rest| rest.split(['/', ':']).next())
        .map(|host| {
            host.eq_ignore_ascii_case("localhost")
                || host == "127.0.0.1"
                || host == "[::1]"
                || host == "::1"
        })
        .unwrap_or(false)
}

/// An event delivered by the browser, tagged with the originating session.
#[derive(Debug, Clone)]
pub struct CdpEvent {
    pub session_id: Option<String>,
    pub method: String,
    pub params: serde_json::Value,
}

/// A live connection to the browser debugging endpoint.
pub struct CdpConnection {
    sink: futures_util::stream::SplitSink<WebSocketStream<MaybeTlsStream<TcpStream>>, Message>,
    requests: Arc<Mutex<HashMap<u64, oneshot::Sender<serde_json::Value>>>>,
    events: broadcast::Sender<CdpEvent>,
    next_id: AtomicU64,
    reader: tokio::task::JoinHandle<()>,
}

impl CdpConnection {
    /// Open a WebSocket to `ws_url` (loopback only) and start the reader.
    pub async fn connect(ws_url: &str) -> Result<Self, WinkitError> {
        // Refuse non-loopback targets; CDP must never be dialed remotely.
        if !ws_is_loopback(ws_url) {
            return Err(WinkitError::protocol(
                "CDP client refuses non-loopback WebSocket endpoints",
            ));
        }
        let (ws, _resp) = connect_async(ws_url).await.map_err(|e| {
            WinkitError::new(ErrorKind::ProtocolError, "CDP WebSocket connect failed")
                .with_source(e)
        })?;
        let (sink, stream) = ws.split();
        let requests: Arc<Mutex<HashMap<u64, oneshot::Sender<serde_json::Value>>>> =
            Arc::new(Mutex::new(HashMap::new()));
        let (events_tx, _) = broadcast::channel(512);
        let reader = tokio::spawn(Self::read_loop(stream, requests.clone(), events_tx.clone()));
        Ok(Self {
            sink,
            requests,
            events: events_tx,
            next_id: AtomicU64::new(1),
            reader,
        })
    }

    async fn read_loop(
        mut stream: futures_util::stream::SplitStream<WebSocketStream<MaybeTlsStream<TcpStream>>>,
        requests: Arc<Mutex<HashMap<u64, oneshot::Sender<serde_json::Value>>>>,
        events: broadcast::Sender<CdpEvent>,
    ) {
        while let Some(msg) = stream.next().await {
            match msg {
                Ok(Message::Text(text)) => {
                    let Ok(value) = serde_json::from_str::<serde_json::Value>(&text) else {
                        continue;
                    };
                    if let Some(id) = value.get("id").and_then(|v| v.as_u64()) {
                        if let Some(tx) = requests.lock().await.remove(&id) {
                            let _ = tx.send(value);
                        }
                    } else if let Some(method) = value.get("method").and_then(|v| v.as_str()) {
                        let ev = CdpEvent {
                            session_id: value
                                .get("sessionId")
                                .and_then(|v| v.as_str())
                                .map(|s| s.to_string()),
                            method: method.to_string(),
                            params: value
                                .get("params")
                                .cloned()
                                .unwrap_or(serde_json::Value::Null),
                        };
                        let _ = events.send(ev);
                    }
                }
                Ok(Message::Close(_)) => break,
                Ok(Message::Ping(p)) => {
                    let _ = events.send(CdpEvent {
                        session_id: None,
                        method: "ws.ping".to_string(),
                        params: serde_json::Value::Null,
                    });
                    // The tungstenite stack auto-answers pings.
                    let _ = p;
                }
                Err(_) => break,
                _ => {}
            }
        }
    }

    /// Send one CDP command and await its result. `session_id` multiplexes
    /// the call onto an attached target when present.
    pub async fn call(
        &mut self,
        method: &str,
        params: serde_json::Value,
        session_id: Option<&str>,
    ) -> Result<serde_json::Value, WinkitError> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = oneshot::channel();
        self.requests.lock().await.insert(id, tx);
        let mut msg = serde_json::json!({ "id": id, "method": method });
        if let Some(sid) = session_id {
            msg["sessionId"] = serde_json::Value::String(sid.to_string());
        }
        if !params.is_null() {
            msg["params"] = params;
        }
        let text = serde_json::to_string(&msg)?;
        let send_result = self.sink.send(Message::Text(text.into())).await;
        if send_result.is_err() {
            self.requests.lock().await.remove(&id);
            return Err(WinkitError::protocol("CDP socket closed while sending"));
        }
        let response = match tokio::time::timeout(CDP_CALL_TIMEOUT, rx).await {
            Ok(Ok(value)) => value,
            Ok(Err(_)) => return Err(WinkitError::protocol("CDP response channel closed")),
            Err(_) => {
                self.requests.lock().await.remove(&id);
                return Err(WinkitError::timeout(format!(
                    "CDP command '{method}' timed out"
                )));
            }
        };
        if let Some(err) = response.get("error") {
            return Err(WinkitError::protocol(format!(
                "CDP error for '{method}': {}",
                err
            )));
        }
        Ok(response
            .get("result")
            .cloned()
            .unwrap_or(serde_json::Value::Null))
    }

    /// Subscribe to CDP events.
    pub fn subscribe(&self) -> broadcast::Receiver<CdpEvent> {
        self.events.subscribe()
    }

    /// Gracefully close the connection.
    pub async fn close(&mut self) {
        let _ = self.sink.send(Message::Close(None)).await;
        self.reader.abort();
    }
}

/// Drain events from a broadcast receiver for up to `duration`, keeping only
/// events for `session_id` (if given). Returns them all.
pub async fn collect_events(
    mut rx: broadcast::Receiver<CdpEvent>,
    session_id: Option<&str>,
    duration: Duration,
) -> Vec<CdpEvent> {
    let mut out = Vec::new();
    let deadline = tokio::time::Instant::now() + duration;
    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            break;
        }
        match tokio::time::timeout(remaining, rx.recv()).await {
            Ok(Ok(ev)) => {
                if session_id
                    .map(|s| ev.session_id.as_deref() == Some(s))
                    .unwrap_or(true)
                {
                    out.push(ev);
                }
            }
            _ => break,
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn refuses_remote_websocket_endpoints() {
        let url = "ws://evil.example.com:9222/devtools/browser/x";
        let fut = CdpConnection::connect(url);
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        match rt.block_on(fut) {
            Err(e) => assert_eq!(e.kind, ErrorKind::ProtocolError),
            Ok(_) => panic!("expected the connection attempt to be refused"),
        }
    }
}
