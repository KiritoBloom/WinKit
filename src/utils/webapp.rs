//! Bounded HTTP probing for local web applications (§9.4).
//!
//! `probe_url` issues a single GET against a validated local URL and reports
//! status, timing, content type, redirect behavior, and connection errors —
//! never response bodies by default. Redirect targets are re-validated
//! against the local-URL policy so a local probe can never be silently
//! redirected to an external host. TLS connections are established with
//! certificate validation disabled (matching the documented `[web]`
//! `local_tls_allowed` posture: local TLS failures are *reported*, not
//! silently accepted as trust).
//!
//! The probe is synchronous and bounded by socket timeouts; callers wrap it
//! in `tokio::task::spawn_blocking` + an absolute deadline.

use crate::errors::{ErrorKind, WinkitError};
use crate::utils::redact::{redact_url_userinfo, redact_value};
use crate::utils::url::{resolve_redirect, validate_redirect_location, UrlPolicy, ValidatedUrl};
use std::io::{Read, Write};
use std::net::TcpStream;
use std::time::{Duration, Instant};

/// Cap on the status/header block of a probe response.
const MAX_HEADER_BYTES: usize = 64 * 1024;

/// Outcomes of a probe, used to classify local-web-app failures.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProbeOutcome {
    Ok,
    HttpError,
    Redirect,
    RedirectLoop,
    RedirectToExternalBlocked,
    ConnectionRefused,
    ConnectionTimeout,
    DnsError,
    Unreachable,
    TlsError,
    MalformedResponse,
    BodyTooLarge,
    TooManyRedirects,
}

impl ProbeOutcome {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Ok => "ok",
            Self::HttpError => "http_error",
            Self::Redirect => "redirect",
            Self::RedirectLoop => "redirect_loop",
            Self::RedirectToExternalBlocked => "redirect_to_external_blocked",
            Self::ConnectionRefused => "connection_refused",
            Self::ConnectionTimeout => "connection_timeout",
            Self::DnsError => "dns_error",
            Self::Unreachable => "unreachable",
            Self::TlsError => "tls_error",
            Self::MalformedResponse => "malformed_response",
            Self::BodyTooLarge => "body_too_large",
            Self::TooManyRedirects => "too_many_redirects",
        }
    }
}

/// A single bounded probe result.
#[derive(Debug, Clone)]
pub struct ProbeResult {
    /// Sanitized final URL after redirects (userinfo stripped).
    pub url: String,
    pub outcome: ProbeOutcome,
    /// HTTP status of the final response (when a response was received).
    pub status: Option<u16>,
    /// Content-Type of the final response, truncated.
    pub content_type: Option<String>,
    /// Redirect chain status codes.
    pub redirects: Vec<u16>,
    /// Bytes of the response body read (never exceeds `max_body_bytes`).
    pub body_bytes: usize,
    /// True when the body was truncated at the byte cap.
    pub body_truncated: bool,
    /// Bounded, redacted body preview (empty by default).
    pub body: Vec<u8>,
    pub elapsed_ms: u64,
    /// Human explanation of the outcome for error classification.
    pub detail: Option<String>,
}

impl ProbeResult {
    /// True when the probe successfully reached an HTTP application and got
    /// a final response (regardless of 4xx/5xx status).
    pub fn reached_http(&self) -> bool {
        self.outcome == ProbeOutcome::Ok || self.outcome == ProbeOutcome::HttpError
    }
}

/// Probe parameters.
#[derive(Debug, Clone)]
pub struct ProbeConfig {
    /// Absolute per-attempt deadline (ms).
    pub timeout_ms: u64,
    /// Cap on the response body read (bytes).
    pub max_body_bytes: usize,
    /// Maximum redirect hops followed.
    pub max_redirects: usize,
    /// Whether to capture a redacted body preview.
    pub capture_body: bool,
    /// Redirect policy applied to every hop.
    pub policy: UrlPolicy,
}

impl ProbeConfig {
    pub fn from_config(web: &crate::config::WebConfig) -> Self {
        Self {
            timeout_ms: web.max_http_ms,
            max_body_bytes: web.max_http_bytes,
            max_redirects: web.max_redirects,
            capture_body: false,
            policy: UrlPolicy::from_config(web),
        }
    }
}

/// Probe a validated local URL. Connection, request, and redirects all share
/// the configured deadline.
pub fn probe_url(validated: &ValidatedUrl, config: &ProbeConfig) -> ProbeResult {
    let start = Instant::now();
    let deadline = Duration::from_millis(config.timeout_ms.max(100));
    let mut current = validated.clone();
    let mut redirects: Vec<u16> = Vec::new();
    let mut hops = 0usize;

    loop {
        let remaining = deadline.saturating_sub(start.elapsed());
        if remaining.is_zero() {
            return finished(
                current,
                ProbeOutcome::ConnectionTimeout,
                None,
                redirects,
                (Vec::new(), false),
                start.elapsed().as_millis() as u64,
                Some("probe deadline expired".to_string()),
            );
        }
        match request_once(&current, remaining, config) {
            ProbeAttempt::Response(status, content_type, head, body, truncated) => {
                if is_redirect(status) {
                    if hops >= config.max_redirects {
                        return finished(
                            current,
                            ProbeOutcome::TooManyRedirects,
                            Some((status, content_type)),
                            redirects,
                            (Vec::new(), false),
                            start.elapsed().as_millis() as u64,
                            Some("too many redirect hops".to_string()),
                        );
                    }
                    redirects.push(status);
                    hops += 1;
                    match resolve_redirect_from_response(&current, &head, config) {
                        Ok(next) => {
                            current = next;
                            continue;
                        }
                        Err(e) => {
                            let outcome = if e.kind == ErrorKind::UrlRejected {
                                ProbeOutcome::RedirectToExternalBlocked
                            } else {
                                ProbeOutcome::MalformedResponse
                            };
                            return finished(
                                current,
                                outcome,
                                Some((status, content_type)),
                                redirects,
                                (Vec::new(), false),
                                start.elapsed().as_millis() as u64,
                                Some(e.message),
                            );
                        }
                    }
                }
                let outcome = if (400..600).contains(&status) {
                    ProbeOutcome::HttpError
                } else {
                    ProbeOutcome::Ok
                };
                return finished(
                    current,
                    outcome,
                    Some((status, content_type)),
                    redirects,
                    (body, truncated),
                    start.elapsed().as_millis() as u64,
                    None,
                );
            }
            ProbeAttempt::Error(outcome, detail) => {
                return finished(
                    current,
                    outcome,
                    None,
                    redirects,
                    (Vec::new(), false),
                    start.elapsed().as_millis() as u64,
                    Some(detail),
                );
            }
        }
    }
}

/// Read the `Location` header out of the raw response head and resolve it.
fn resolve_redirect_from_response(
    base: &ValidatedUrl,
    head: &str,
    config: &ProbeConfig,
) -> Result<ValidatedUrl, WinkitError> {
    let location = head
        .lines()
        .find_map(|line| {
            let (name, value) = line.split_once(':')?;
            if name.trim().eq_ignore_ascii_case("location") {
                Some(value.trim().to_string())
            } else {
                None
            }
        })
        .ok_or_else(|| WinkitError::protocol("redirect response carried no Location header"))?;
    // Validate the raw location against the policy first (blocks external
    // escapes), then resolve relative targets against the base.
    if location.starts_with("http://") || location.starts_with("https://") {
        validate_redirect_location(&location, &config.policy)
    } else {
        resolve_redirect(base, &location, &config.policy)
    }
}

enum ProbeAttempt {
    Response(u16, Option<String>, String, Vec<u8>, bool),
    Error(ProbeOutcome, String),
}

fn request_once(validated: &ValidatedUrl, timeout: Duration, config: &ProbeConfig) -> ProbeAttempt {
    let addr = match resolve_addr(validated) {
        Ok(a) => a,
        Err(e) => return ProbeAttempt::Error(ProbeOutcome::DnsError, e),
    };

    let stream = match TcpStream::connect_timeout(&addr, timeout) {
        Ok(s) => s,
        Err(e) => {
            return ProbeAttempt::Error(classify_connect_error(&e), e.to_string());
        }
    };
    let _ = stream.set_read_timeout(Some(timeout));
    let _ = stream.set_write_timeout(Some(timeout));

    // Wrap in TLS when the scheme requires it.
    let mut stream: Box<dyn ReadWrite> = if validated.scheme == "https" {
        match tls_wrap(stream, validated) {
            Ok(s) => Box::new(s),
            Err(e) => {
                return ProbeAttempt::Error(
                    ProbeOutcome::TlsError,
                    format!("TLS handshake failed: {e}"),
                )
            }
        }
    } else {
        Box::new(stream)
    };

    let request = format!(
        "GET {path} HTTP/1.1\r\nHost: {host}\r\nConnection: close\r\nAccept: */*\r\nUser-Agent: winkit/{ver}\r\n\r\n",
        path = validated.path_and_query,
        host = validated.host_header,
        ver = env!("CARGO_PKG_VERSION"),
    );
    if stream.write_all(request.as_bytes()).is_err() {
        return ProbeAttempt::Error(ProbeOutcome::Unreachable, "request write failed".into());
    }
    let _ = stream.flush();

    let mut raw = Vec::new();
    let mut buf = [0u8; 8192];
    // Read the header block.
    let header_end = loop {
        if let Some(pos) = raw.windows(4).position(|w| w == b"\r\n\r\n") {
            break pos;
        }
        if raw.len() > MAX_HEADER_BYTES {
            return ProbeAttempt::Error(
                ProbeOutcome::MalformedResponse,
                "headers exceeded the size cap".into(),
            );
        }
        match stream.read(&mut buf) {
            Ok(0) => {
                if raw.is_empty() {
                    return ProbeAttempt::Error(
                        ProbeOutcome::Unreachable,
                        "connection closed before any bytes".into(),
                    );
                }
                return ProbeAttempt::Error(
                    ProbeOutcome::MalformedResponse,
                    "connection closed before headers completed".into(),
                );
            }
            Ok(n) => raw.extend_from_slice(&buf[..n]),
            Err(e) => {
                let detail = e.to_string();
                return ProbeAttempt::Error(
                    classify_read_error(&e),
                    format!("read failed: {detail}"),
                );
            }
        }
    };

    let head = String::from_utf8_lossy(&raw[..header_end]);
    let status = match parse_status(&head) {
        Some(s) => s,
        None => {
            return ProbeAttempt::Error(
                ProbeOutcome::MalformedResponse,
                "malformed status line".into(),
            )
        }
    };
    let content_type = parse_header(&head, "content-type").map(|v| crate::utils::truncate(v, 120));

    // Read the body, bounded by the cap. For redirect responses we only need
    // the headers, so a minimal body read is fine.
    let body_start = header_end + 4;
    let content_length =
        parse_header(&head, "content-length").and_then(|v| v.trim().parse::<usize>().ok());
    let mut body: Vec<u8> = raw[body_start..].to_vec();
    let truncated = match content_length {
        Some(len) => {
            let want = len.min(config.max_body_bytes + 1);
            while body.len() < want {
                match stream.read(&mut buf) {
                    Ok(0) => break,
                    Ok(n) => body.extend_from_slice(&buf[..n]),
                    Err(e) => {
                        let detail = e.to_string();
                        return ProbeAttempt::Error(
                            classify_read_error(&e),
                            format!("read failed: {detail}"),
                        );
                    }
                }
            }
            len > config.max_body_bytes
        }
        None => {
            while body.len() <= config.max_body_bytes {
                match stream.read(&mut buf) {
                    Ok(0) => break,
                    Ok(n) => body.extend_from_slice(&buf[..n]),
                    Err(e) => {
                        let detail = e.to_string();
                        return ProbeAttempt::Error(
                            classify_read_error(&e),
                            format!("read failed: {detail}"),
                        );
                    }
                }
            }
            body.len() > config.max_body_bytes
        }
    };
    if truncated {
        body.truncate(config.max_body_bytes);
    }

    // Never surface a full body: cap and redact previews.
    if config.capture_body {
        let preview: String = String::from_utf8_lossy(&body).into_owned();
        body = redact_value(&crate::utils::truncate(&preview, 2000)).into_bytes();
    } else {
        body.clear();
    }

    ProbeAttempt::Response(status, content_type, head.to_string(), body, truncated)
}

fn finished(
    url: ValidatedUrl,
    outcome: ProbeOutcome,
    response: Option<(u16, Option<String>)>,
    redirects: Vec<u16>,
    body_payload: (Vec<u8>, bool),
    elapsed_ms: u64,
    detail: Option<String>,
) -> ProbeResult {
    let (body, body_truncated) = body_payload;
    let (status, content_type) = match response {
        Some((s, ct)) => (Some(s), ct),
        None => (None, None),
    };
    ProbeResult {
        url: redact_url_userinfo(&url.display()),
        outcome,
        status,
        content_type,
        redirects,
        body_bytes: body.len(),
        body_truncated,
        body,
        elapsed_ms,
        detail,
    }
}

fn resolve_addr(validated: &ValidatedUrl) -> Result<std::net::SocketAddr, String> {
    let ip = if validated.host.eq_ignore_ascii_case("localhost") {
        std::net::IpAddr::V4(std::net::Ipv4Addr::LOCALHOST)
    } else {
        validated
            .host
            .parse::<std::net::IpAddr>()
            .map_err(|_| format!("host '{}' does not resolve to a literal IP", validated.host))?
    };
    Ok(std::net::SocketAddr::new(ip, validated.port))
}

fn tls_wrap(
    stream: TcpStream,
    validated: &ValidatedUrl,
) -> Result<native_tls::TlsStream<TcpStream>, String> {
    // Local TLS is *reported*, never trusted: accept any certificate and any
    // hostname so a handshake failure is surfaced as TLS evidence rather
    // than a validation error.
    let connector = native_tls::TlsConnector::builder()
        .danger_accept_invalid_certs(true)
        .danger_accept_invalid_hostnames(true)
        .build()
        .map_err(|e| e.to_string())?;
    connector
        .connect(&validated.host, stream)
        .map_err(|e| match e {
            native_tls::HandshakeError::Failure(f) => f.to_string(),
            native_tls::HandshakeError::WouldBlock(_) => {
                "TLS handshake interrupted (WouldBlock) on a blocking socket".to_string()
            }
        })
}

trait ReadWrite: Read + Write {}
impl<T: Read + Write> ReadWrite for T {}

fn is_redirect(status: u16) -> bool {
    matches!(status, 301 | 302 | 303 | 307 | 308)
}

fn parse_status(head: &str) -> Option<u16> {
    head.lines()
        .next()?
        .split_whitespace()
        .nth(1)?
        .parse::<u16>()
        .ok()
}

fn parse_header<'a>(head: &'a str, name: &str) -> Option<&'a str> {
    head.lines().find_map(|line| {
        let (n, v) = line.split_once(':')?;
        if n.trim().eq_ignore_ascii_case(name) {
            Some(v.trim())
        } else {
            None
        }
    })
}

fn classify_read_error(e: &std::io::Error) -> ProbeOutcome {
    match e.kind() {
        std::io::ErrorKind::TimedOut => ProbeOutcome::ConnectionTimeout,
        // Windows reports refused connections as ConnectionReset.
        std::io::ErrorKind::ConnectionReset => ProbeOutcome::ConnectionRefused,
        _ => ProbeOutcome::Unreachable,
    }
}

fn classify_connect_error(e: &std::io::Error) -> ProbeOutcome {
    match e.kind() {
        std::io::ErrorKind::ConnectionRefused | std::io::ErrorKind::ConnectionReset => {
            ProbeOutcome::ConnectionRefused
        }
        std::io::ErrorKind::TimedOut => ProbeOutcome::ConnectionTimeout,
        std::io::ErrorKind::HostUnreachable
        | std::io::ErrorKind::NetworkUnreachable
        | std::io::ErrorKind::AddrNotAvailable => ProbeOutcome::Unreachable,
        _ => ProbeOutcome::Unreachable,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpListener;
    use std::sync::Mutex;

    /// Serialize socket tests so ephemeral ports are never reused by a
    /// concurrent test (Windows RST behavior is timing-sensitive).
    static PORT_GUARD: Mutex<()> = Mutex::new(());

    fn cfg() -> ProbeConfig {
        ProbeConfig {
            timeout_ms: 2000,
            max_body_bytes: 4096,
            max_redirects: 5,
            capture_body: false,
            policy: UrlPolicy {
                allow_external: false,
                dev_hosts: Vec::new(),
                local_tls_allowed: true,
            },
        }
    }

    fn serve(response: &'static str) -> (String, std::thread::JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let url = format!("http://127.0.0.1:{}/", addr.port());
        let handle = std::thread::spawn(move || {
            if let Ok((mut sock, _)) = listener.accept() {
                let mut req = [0u8; 2048];
                let _ = sock.read(&mut req);
                let _ = sock.write_all(response.as_bytes());
                let _ = sock.flush();
                std::thread::sleep(std::time::Duration::from_millis(100));
            }
        });
        (url, handle)
    }

    fn validated(url: &str) -> ValidatedUrl {
        crate::utils::url::validate_url(url, &cfg().policy).unwrap()
    }

    #[test]
    fn probes_http_success() {
        let _g = PORT_GUARD.lock().unwrap();
        let (url, h) =
            serve("HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: 5\r\n\r\nhello");
        let r = probe_url(&validated(&url), &cfg());
        h.join().unwrap();
        assert_eq!(r.outcome, ProbeOutcome::Ok);
        assert_eq!(r.status, Some(200));
        assert_eq!(r.content_type.as_deref(), Some("text/html"));
        assert!(r.body_bytes == 0); // body not captured by default
        assert_eq!(r.redirects.len(), 0);
    }

    #[test]
    fn probes_http_error_status() {
        let _g = PORT_GUARD.lock().unwrap();
        let (url, h) = serve("HTTP/1.1 500 Internal Server Error\r\nContent-Length: 0\r\n\r\n");
        let r = probe_url(&validated(&url), &cfg());
        h.join().unwrap();
        assert_eq!(r.outcome, ProbeOutcome::HttpError);
        assert_eq!(r.status, Some(500));
    }

    #[test]
    fn follows_local_redirect_within_policy() {
        let _g = PORT_GUARD.lock().unwrap();
        // Self-redirecting loop: redirects are followed and re-validated
        // until the hop cap is reached.
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let handle = std::thread::spawn(move || {
            let mut conns = 0;
            while conns < 6 {
                if let Ok((mut sock, _)) = listener.accept() {
                    conns += 1;
                    let mut req = [0u8; 2048];
                    let _ = sock.read(&mut req);
                    let body = format!(
                        "HTTP/1.1 302 Found\r\nLocation: http://127.0.0.1:{}/self\r\nContent-Length: 0\r\n\r\n",
                        port
                    );
                    let _ = sock.write_all(body.as_bytes());
                }
            }
        });
        let url = format!("http://127.0.0.1:{port}/self");
        let r = probe_url(&validated(&url), &cfg());
        handle.join().unwrap();
        assert_eq!(r.outcome, ProbeOutcome::TooManyRedirects);
        assert_eq!(r.redirects.len(), 5);
    }

    #[test]
    fn blocks_redirect_to_external_host() {
        let _g = PORT_GUARD.lock().unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let handle = std::thread::spawn(move || {
            if let Ok((mut sock, _)) = listener.accept() {
                let mut req = [0u8; 2048];
                let _ = sock.read(&mut req);
                let body = "HTTP/1.1 302 Found\r\nLocation: http://example.com/x\r\nContent-Length: 0\r\n\r\n";
                let _ = sock.write_all(body.as_bytes());
            }
        });
        let url = format!("http://127.0.0.1:{port}/start");
        let r = probe_url(&validated(&url), &cfg());
        handle.join().unwrap();
        assert_eq!(r.outcome, ProbeOutcome::RedirectToExternalBlocked);
    }

    #[test]
    fn connection_refused_is_classified() {
        let _g = PORT_GUARD.lock().unwrap();
        // Bind then drop to free the port, then probe it. Windows loopback
        // reports a closed port either as refused or as a connect timeout,
        // so accept both "nothing is listening" outcomes here.
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        drop(listener);
        let url = format!("http://127.0.0.1:{port}/");
        let r = probe_url(&validated(&url), &cfg());
        assert!(
            matches!(
                r.outcome,
                ProbeOutcome::ConnectionRefused | ProbeOutcome::ConnectionTimeout
            ),
            "unexpected outcome: {:?}",
            r.outcome
        );
    }

    #[test]
    fn classifies_connect_errors() {
        use std::io::ErrorKind;
        assert_eq!(
            classify_connect_error(&std::io::Error::new(ErrorKind::ConnectionRefused, "x")),
            ProbeOutcome::ConnectionRefused
        );
        assert_eq!(
            classify_connect_error(&std::io::Error::new(ErrorKind::ConnectionReset, "x")),
            ProbeOutcome::ConnectionRefused
        );
        assert_eq!(
            classify_connect_error(&std::io::Error::new(ErrorKind::TimedOut, "x")),
            ProbeOutcome::ConnectionTimeout
        );
        assert_eq!(
            classify_connect_error(&std::io::Error::new(ErrorKind::AddrNotAvailable, "x")),
            ProbeOutcome::Unreachable
        );
        assert_eq!(
            classify_read_error(&std::io::Error::new(ErrorKind::ConnectionReset, "x")),
            ProbeOutcome::ConnectionRefused
        );
    }

    #[test]
    fn body_preview_is_bounded_and_redacted() {
        let _g = PORT_GUARD.lock().unwrap();
        let (url, h) = serve(
            "HTTP/1.1 200 OK\r\nContent-Length: 50\r\n\r\nsk-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
        );
        let mut c = cfg();
        c.capture_body = true;
        c.max_body_bytes = 64;
        let r = probe_url(&validated(&url), &c);
        h.join().unwrap();
        let body = String::from_utf8_lossy(&r.body);
        assert!(!body.contains("aaaaaaaaaaaaaaaaaaaaaaaa"));
        assert!(body.contains("sk-<redacted>"));
    }
}
