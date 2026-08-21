//! Minimal HTTP/1.1 GET client used for local web-app probes.
//!
//! This exists instead of pulling in a full HTTP stack because WinKit only
//! needs a handful of bounded `GET` calls against loopback servers. Requests
//! are bounded by size, count, and timeout, and only allowlisted hosts are
//! ever connected to.

use crate::errors::{ErrorKind, WinkitError};
use std::io::Read as _;
use std::net::{SocketAddr, TcpStream};
use std::time::Duration;

/// The response to a bounded GET request.
#[derive(Debug, Clone)]
pub struct HttpGetResponse {
    pub status: u16,
    pub body: String,
}

/// Upper bound on response headers (far beyond any DevTools response).
const MAX_HEADER_BYTES: usize = 64 * 1024;

/// Perform a bounded HTTP GET against a loopback address.
///
/// * `max_body_bytes` bounds how much of the response body is read.
/// * `timeout` bounds the whole operation.
/// * Only loopback addresses are accepted.
///
/// The body is read by `Content-Length` when present. Chrome's DevTools
/// server keeps the connection open even when asked to close it, so reading
/// until EOF would block until the socket timeout and fail discovery.
pub fn http_get(
    addr: SocketAddr,
    path: &str,
    timeout: Duration,
    max_body_bytes: usize,
) -> Result<HttpGetResponse, WinkitError> {
    if !addr.ip().is_loopback() {
        return Err(WinkitError::protocol(
            "HTTP client refuses non-loopback connections",
        ));
    }
    let stream = TcpStream::connect_timeout(&addr, timeout).map_err(|e| {
        WinkitError::new(ErrorKind::ProtocolError, "TCP connect failed").with_source(e)
    })?;
    stream.set_read_timeout(Some(timeout)).map_err(|e| {
        WinkitError::new(ErrorKind::ProtocolError, "set_read_timeout failed").with_source(e)
    })?;

    let request = format!(
        "GET {path} HTTP/1.1\r\nHost: {}\r\nConnection: close\r\nUser-Agent: winkit/{}\r\n\r\n",
        addr,
        env!("CARGO_PKG_VERSION")
    );
    let mut stream = stream;
    std::io::Write::write_all(&mut stream, request.as_bytes()).map_err(|e| {
        WinkitError::new(ErrorKind::ProtocolError, "request write failed").with_source(e)
    })?;

    let mut raw = Vec::new();
    let mut buf = [0u8; 4096];
    let header_end = loop {
        if let Some(pos) = raw.windows(4).position(|w| w == b"\r\n\r\n") {
            break pos;
        }
        if raw.len() > MAX_HEADER_BYTES {
            return Err(WinkitError::protocol(
                "malformed HTTP response: headers too large",
            ));
        }
        let n = stream.read(&mut buf).map_err(|e| {
            WinkitError::new(ErrorKind::ProtocolError, "response read failed").with_source(e)
        })?;
        if n == 0 {
            return Err(WinkitError::protocol(
                "malformed HTTP response: connection closed before headers",
            ));
        }
        raw.extend_from_slice(&buf[..n]);
    };

    let head = String::from_utf8_lossy(&raw[..header_end]);
    let content_length = head.lines().find_map(|line| {
        let (name, value) = line.split_once(':')?;
        if name.trim().eq_ignore_ascii_case("content-length") {
            value.trim().parse::<usize>().ok()
        } else {
            None
        }
    });

    let body_start = header_end + 4;
    match content_length {
        Some(len) => {
            let want = (body_start + len).min(body_start + max_body_bytes);
            while raw.len() < want {
                let n = stream.read(&mut buf).map_err(|e| {
                    WinkitError::new(ErrorKind::ProtocolError, "response read failed")
                        .with_source(e)
                })?;
                if n == 0 {
                    break;
                }
                raw.extend_from_slice(&buf[..n]);
            }
        }
        None => {
            // No length declared: read until the server closes, bounded by
            // the socket timeout and the payload cap.
            while let Ok(n) = stream.read(&mut buf) {
                if n == 0 {
                    break;
                }
                raw.extend_from_slice(&buf[..n]);
                if raw.len() > body_start + max_body_bytes {
                    break;
                }
            }
        }
    }

    parse_http_response(&raw)
}

fn parse_http_response(raw: &[u8]) -> Result<HttpGetResponse, WinkitError> {
    let header_end = raw
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .ok_or_else(|| {
            WinkitError::protocol("malformed HTTP response: missing header terminator")
        })?;
    let head = String::from_utf8_lossy(&raw[..header_end]);
    let mut lines = head.lines();
    let status_line = lines
        .next()
        .ok_or_else(|| WinkitError::protocol("malformed HTTP response: missing status line"))?;
    let status = status_line
        .split_whitespace()
        .nth(1)
        .and_then(|s| s.parse::<u16>().ok())
        .ok_or_else(|| WinkitError::protocol("malformed HTTP response: bad status code"))?;
    let body = String::from_utf8_lossy(&raw[header_end + 4..]).into_owned();
    Ok(HttpGetResponse { status, body })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_simple_http_response() {
        let raw = b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\n\r\n{\"ok\":true}";
        let resp = parse_http_response(raw).unwrap();
        assert_eq!(resp.status, 200);
        assert_eq!(resp.body, "{\"ok\":true}");
    }

    #[test]
    fn reads_body_by_content_length_even_when_server_keeps_connection_open() {
        use std::io::Write as _;
        use std::net::TcpListener;
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            let (mut sock, _) = listener.accept().unwrap();
            let mut req = [0u8; 2048];
            let _ = sock.read(&mut req);
            sock.write_all(
                b"HTTP/1.1 200 OK\r\nContent-Length: 12\r\nContent-Type: application/json\r\n\r\n{\"ok\":true}",
            )
            .unwrap();
            // Mimic Chrome's DevTools server: it never closes the connection,
            // so a client reading until EOF would block until its timeout.
            std::thread::sleep(std::time::Duration::from_millis(500));
        });
        let resp = http_get(addr, "/json/version", Duration::from_secs(2), 64 * 1024).unwrap();
        assert_eq!(resp.status, 200);
        assert_eq!(resp.body, "{\"ok\":true}");
        server.join().unwrap();
    }

    #[test]
    fn rejects_non_loopback_addresses() {
        let err = http_get(
            "8.8.8.8:80".parse().unwrap(),
            "/json/version",
            Duration::from_secs(1),
            1024,
        )
        .unwrap_err();
        assert_eq!(err.kind, ErrorKind::ProtocolError);
    }
}
