//! Local URL validation for web-app tooling.
//!
//! WinKit's web tools accept `http`/`https` URLs bound to loopback hosts,
//! `localhost`, and explicitly configured development hosts. Everything else
//! is rejected unless external access is explicitly enabled. Redirect targets
//! are re-validated with the same policy so a local probe can never be
//! silently redirected to an arbitrary external host.

use crate::errors::WinkitError;

/// Maximum length of a URL accepted by WinKit tools.
pub const MAX_URL_CHARS: usize = 2048;

/// The URL policy derived from `[web]` configuration.
#[derive(Debug, Clone)]
pub struct UrlPolicy {
    /// Allow non-loopback, non-dev-host hosts.
    pub allow_external: bool,
    /// Explicitly trusted development hosts (case-insensitive).
    pub dev_hosts: Vec<String>,
    /// Permit `https://` local endpoints (TLS errors are reported, never
    /// validated against the system store).
    pub local_tls_allowed: bool,
}

impl UrlPolicy {
    pub fn from_config(web: &crate::config::WebConfig) -> Self {
        Self {
            allow_external: web.allow_external_urls,
            dev_hosts: web.dev_hosts.clone(),
            local_tls_allowed: web.local_tls_allowed,
        }
    }
}

/// A validated, local-only URL ready for probing or navigation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ValidatedUrl {
    pub scheme: String,
    /// Lowercased host, without brackets for IPv6.
    pub host: String,
    /// Host exactly as it should appear in an HTTP `Host` header (includes
    /// the port only when it is not the scheme default).
    pub host_header: String,
    pub port: u16,
    /// Path plus optional query, defaulting to `/`. Bounded.
    pub path_and_query: String,
    pub is_loopback: bool,
    /// True when the host is loopback or listed in the dev hosts.
    pub is_local: bool,
    /// The original caller-supplied URL.
    pub original: String,
}

impl ValidatedUrl {
    /// A canonical display form without the query string (query strings can
    /// carry tokens).
    pub fn display(&self) -> String {
        format!(
            "{}://{}{}",
            self.scheme,
            self.host_header,
            self.path_no_query()
        )
    }

    fn path_no_query(&self) -> &str {
        self.path_and_query
            .split('?')
            .next()
            .unwrap_or(self.path_and_query.as_str())
    }
}

/// Validate and normalize a URL against the web policy.
///
/// Rejects: non-`http`/`https` schemes (`javascript:`, `data:`, `file:`,
/// `chrome:`, `devtools:`, `ws:`, ...), control characters, malformed
/// authorities, ambiguous URLs (no scheme), and external hosts unless
/// explicitly allowed.
pub fn validate_url(raw: &str, policy: &UrlPolicy) -> Result<ValidatedUrl, WinkitError> {
    let raw = raw.trim();
    if raw.is_empty() {
        return Err(WinkitError::url_rejected("empty URL"));
    }
    if raw.chars().count() > MAX_URL_CHARS {
        return Err(WinkitError::url_rejected(format!(
            "URL exceeds the {MAX_URL_CHARS} character limit"
        )));
    }
    if raw.chars().any(|c| c.is_control()) {
        return Err(WinkitError::url_rejected("URL contains control characters"));
    }

    let (scheme, rest) = raw.split_once("://").ok_or_else(|| {
        WinkitError::url_rejected("URL is ambiguous: include a scheme (http:// or https://)")
    })?;
    let scheme = scheme.trim().to_ascii_lowercase();
    if scheme != "http" && scheme != "https" {
        return Err(WinkitError::url_rejected(format!(
            "unsupported URL scheme '{scheme}' (only http and https are accepted)"
        )));
    }
    if scheme == "https" && !policy.local_tls_allowed {
        return Err(WinkitError::url_rejected(
            "https URLs are disabled by the web.local_tls_allowed = false setting",
        ));
    }

    // Split authority from path.
    let (authority, mut path) = match rest.find('/') {
        Some(idx) => (&rest[..idx], rest[idx..].to_string()),
        None => (rest, "/".to_string()),
    };
    let authority = authority.trim();
    if authority.is_empty() {
        return Err(WinkitError::url_rejected("URL has an empty host"));
    }

    // Parse host and optional port. IPv6 literals are bracketed.
    let (host, port) = parse_authority(authority, &scheme)?;
    let host = host.to_ascii_lowercase();

    if !path.starts_with('/') {
        path.insert(0, '/');
    }
    if path.len() > MAX_URL_CHARS {
        return Err(WinkitError::url_rejected("URL path is too long"));
    }

    let is_loopback = is_loopback_host(&host);
    let is_dev_host = policy
        .dev_hosts
        .iter()
        .any(|d| d.eq_ignore_ascii_case(&host));
    let is_local = is_loopback || is_dev_host;

    if !is_local && !policy.allow_external {
        return Err(WinkitError::url_rejected(format!(
            "host '{host}' is not a loopback or development host and external URLs are disabled \
             (set web.allow_external_urls = true to permit it)"
        )));
    }

    let host_header = match port {
        Some(p) => format!("{}:{p}", bracket_if_v6(&host)),
        None => bracket_if_v6(&host).to_string(),
    };
    let default_port = match scheme.as_str() {
        "https" => 443,
        _ => 80,
    };

    Ok(ValidatedUrl {
        scheme,
        host,
        host_header,
        port: port.unwrap_or(default_port),
        path_and_query: path,
        is_loopback,
        is_local,
        original: raw.to_string(),
    })
}

/// Parse `host[:port]` or `[v6]:port`, returning the host and optional port.
fn parse_authority(authority: &str, scheme: &str) -> Result<(String, Option<u16>), WinkitError> {
    if let Some(rest) = authority.strip_prefix('[') {
        let end = rest.find(']').ok_or_else(|| {
            WinkitError::url_rejected("malformed IPv6 address: missing closing bracket")
        })?;
        let host = &rest[..end];
        if host.is_empty() {
            return Err(WinkitError::url_rejected("empty IPv6 host"));
        }
        let tail = &rest[end + 1..];
        let port = match tail {
            "" => None,
            t if t.starts_with(':') => Some(parse_port(&t[1..])?),
            _ => {
                return Err(WinkitError::url_rejected(
                    "malformed IPv6 authority: unexpected text after ']'",
                ))
            }
        };
        return Ok((host.to_string(), port));
    }

    // Non-bracketed: split at the last colon, but only when the part after
    // it parses as a port. A bare IPv6 literal without brackets is rejected
    // as ambiguous.
    if authority.contains(':') {
        if let Some((host, port_str)) = authority.rsplit_once(':') {
            if host.is_empty() || port_str.is_empty() {
                return Err(WinkitError::url_rejected("malformed host:port"));
            }
            if host.contains(':') {
                return Err(WinkitError::url_rejected(
                    "bare IPv6 addresses are ambiguous; use [::1] notation",
                ));
            }
            return Ok((host.to_string(), Some(parse_port(port_str)?)));
        }
    }
    let _ = scheme;
    Ok((authority.to_string(), None))
}

fn parse_port(s: &str) -> Result<u16, WinkitError> {
    let p = s.parse::<u16>().map_err(|_| {
        WinkitError::url_rejected(format!("invalid port '{s}' (must be 1..=65535)"))
    })?;
    if p == 0 {
        return Err(WinkitError::url_rejected("port 0 is not allowed"));
    }
    Ok(p)
}

fn bracket_if_v6(host: &str) -> String {
    if host.contains(':') {
        format!("[{host}]")
    } else {
        host.to_string()
    }
}

/// Loopback host detection: `localhost`, IPv4 127.0.0.0/8, and IPv6
/// loopback/unspecified forms.
pub fn is_loopback_host(host: &str) -> bool {
    if host.eq_ignore_ascii_case("localhost") {
        return true;
    }
    if let Ok(ip) = host.parse::<std::net::IpAddr>() {
        return ip.is_loopback() || ip.is_unspecified();
    }
    host == "::ffff:127.0.0.1"
}

/// Validate a redirect `Location` header against the same policy as the
/// original request. External redirect targets are blocked unless enabled.
pub fn validate_redirect_location(
    location: &str,
    policy: &UrlPolicy,
) -> Result<ValidatedUrl, WinkitError> {
    validate_url(location, policy).map_err(|_| {
        WinkitError::url_rejected(format!(
            "redirect target '{location}' failed local-URL validation"
        ))
    })
}

/// Build a URL from parts for redirect resolution (relative redirects).
/// The resulting URL is validated against `policy` so a redirect can never
/// escape the local-URL policy.
pub fn resolve_redirect(
    base: &ValidatedUrl,
    location: &str,
    policy: &UrlPolicy,
) -> Result<ValidatedUrl, WinkitError> {
    let full = if location.starts_with('/') {
        format!(
            "{}://{}{}",
            base.scheme,
            bracket_if_v6(&base.host),
            location
        )
    } else if location.starts_with("http://") || location.starts_with("https://") {
        location.to_string()
    } else if location.contains("://") {
        return Err(WinkitError::url_rejected(
            "redirect target uses a non-http(s) scheme",
        ));
    } else {
        // Relative path with no leading slash: resolve against the base
        // path's parent directory (RFC 3986 merge).
        let base_path = base.path_and_query.split('?').next().unwrap_or("/");
        let dir = match base_path.rfind('/') {
            Some(idx) => &base_path[..=idx],
            None => "/",
        };
        format!(
            "{}://{}{}{}",
            base.scheme,
            bracket_if_v6(&base.host),
            dir,
            location
        )
    };
    validate_url(&full, policy).map_err(|e| {
        WinkitError::url_rejected(format!("redirect target '{location}' is invalid: {e}"))
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::errors::ErrorKind;

    fn policy() -> UrlPolicy {
        UrlPolicy {
            allow_external: false,
            dev_hosts: vec!["dev.myapp.test".to_string()],
            local_tls_allowed: true,
        }
    }

    #[test]
    fn accepts_loopback_forms() {
        for raw in [
            "http://localhost:3000",
            "http://127.0.0.1:3000/app",
            "http://[::1]:5173",
            "http://127.0.0.1",
        ] {
            let v = validate_url(raw, &policy()).unwrap();
            assert!(v.is_loopback, "{raw}");
            assert!(v.is_local);
            assert_eq!(
                v.port,
                if raw.ends_with(":5173") {
                    5173
                } else if raw.contains(":3000") {
                    3000
                } else {
                    80
                }
            );
        }
    }

    #[test]
    fn accepts_dev_hosts_when_configured() {
        let v = validate_url("http://dev.myapp.test:8080", &policy()).unwrap();
        assert!(!v.is_loopback);
        assert!(v.is_local);
        assert_eq!(v.port, 8080);
    }

    #[test]
    fn rejects_external_host_by_default() {
        let err = validate_url("http://example.com:80", &policy()).unwrap_err();
        assert_eq!(err.kind, ErrorKind::UrlRejected);
        assert!(err.message.contains("external"));
    }

    #[test]
    fn rejects_unsupported_schemes() {
        for raw in [
            "javascript:alert(1)",
            "data:text/html,hi",
            "file:///C:/x",
            "chrome://settings",
            "devtools://devtools",
            "ws://localhost:3000",
        ] {
            let err = validate_url(raw, &policy()).unwrap_err();
            assert_eq!(err.kind, ErrorKind::UrlRejected, "{raw}");
        }
    }

    #[test]
    fn rejects_ambiguous_and_malformed_urls() {
        assert_eq!(
            validate_url("localhost:3000", &policy()).unwrap_err().kind,
            ErrorKind::UrlRejected
        );
        assert_eq!(
            validate_url("http://:3000", &policy()).unwrap_err().kind,
            ErrorKind::UrlRejected
        );
        assert_eq!(
            validate_url("http://localhost:0", &policy())
                .unwrap_err()
                .kind,
            ErrorKind::UrlRejected
        );
        assert_eq!(
            validate_url("http://[::1", &policy()).unwrap_err().kind,
            ErrorKind::UrlRejected
        );
        assert_eq!(
            validate_url("http://host:99999", &policy())
                .unwrap_err()
                .kind,
            ErrorKind::UrlRejected
        );
    }

    #[test]
    fn rejects_control_characters() {
        assert_eq!(
            validate_url("http://localhost:3000/\u{0000}", &policy())
                .unwrap_err()
                .kind,
            ErrorKind::UrlRejected
        );
    }

    #[test]
    fn https_requires_tls_policy() {
        let mut p = policy();
        p.local_tls_allowed = false;
        assert_eq!(
            validate_url("https://localhost:3000", &p).unwrap_err().kind,
            ErrorKind::UrlRejected
        );
        assert!(validate_url("https://localhost:3000", &policy()).is_ok());
    }

    #[test]
    fn default_ports_by_scheme() {
        let http = validate_url("http://localhost", &policy()).unwrap();
        assert_eq!(http.port, 80);
        assert_eq!(http.host_header, "localhost");
        let https = validate_url("https://localhost", &policy()).unwrap();
        assert_eq!(https.port, 443);
    }

    #[test]
    fn v6_host_header_is_bracketed() {
        let v = validate_url("http://[::1]:5173/x", &policy()).unwrap();
        assert_eq!(v.host_header, "[::1]:5173");
        assert_eq!(v.host, "::1");
    }

    #[test]
    fn display_strips_query_string() {
        let v = validate_url("http://localhost:3000/app?token=secret", &policy()).unwrap();
        assert!(!v.display().contains("token"));
        assert!(v.display().contains("/app"));
    }

    #[test]
    fn relative_redirect_resolution() {
        let base = validate_url("http://localhost:3000/app", &policy()).unwrap();
        let target = resolve_redirect(&base, "/other", &policy()).unwrap();
        assert!(target.path_and_query.starts_with("/other"));
        // RFC 3986 merge: relative "page" resolves against the parent dir.
        let target2 = resolve_redirect(&base, "page", &policy()).unwrap();
        assert!(target2.path_and_query.starts_with("/page"));
        let base3 = validate_url("http://localhost:3000/app/x", &policy()).unwrap();
        let target3 = resolve_redirect(&base3, "y", &policy()).unwrap();
        assert!(target3.path_and_query.starts_with("/app/y"));
    }
}
