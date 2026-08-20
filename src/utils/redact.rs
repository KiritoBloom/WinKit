//! Secret redaction.
//!
//! WinKit never exposes cookies, authorization headers, request bodies, form
//! values, credentials, tokens, raw environment blocks, private keys, or
//! secret-bearing command lines. This module provides a
//! deterministic, best-effort masker for the few surfaces where a caller-
//! or provider-supplied string could smuggle a secret (URLs, command lines,
//! configuration values).
//!
//! Redaction is a defense-in-depth layer, not a license to collect secrets:
//! WinKit's tools do not read `.env`, credential stores, SSH keys, or cloud
//! credentials in the first place.

/// Mask a value that looks like a secret, leaving the rest of `s` intact.
/// The masked prefix keeps enough shape for an agent to reason about *what*
/// was redacted without revealing the secret.
pub fn redact_value(s: &str) -> String {
    redact_secret_literals(s)
}

/// Redact a string recursively, masking known secret-bearing substrings.
///
/// Recognized patterns (each replaced with `<redacted>`):
///   * OpenID / platform tokens: `sk-…`, `ghp_…`, `gho_…`, `github_pat_…`,
///     `xoxb-…`, `xoxp-…`, `xoxa-…`, `glpat-…`, `AKIA…`, `ASIA…`
///   * PEM private keys: `-----BEGIN … PRIVATE KEY-----`
///   * Named values: `password=`, `passwd=`, `token=`, `api_key=`,
///     `apikey=`, `secret=`, `client_secret=`, `access_token=`,
///     `refresh_token=`, `private_key=`, `authorization=`
///   * Authorization headers: `authorization:` and `Bearer …`
///   * URL userinfo: `scheme://user:pass@host`
///   * Connection strings: `…://user:password@host/…`
fn redact_secret_literals(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while !rest.is_empty() {
        // 1. PEM private key blocks.
        if let Some(start) = rest.find("-----BEGIN ") {
            out.push_str(&rest[..start]);
            if let Some(end_rel) = rest[start..].find("-----END ") {
                let block_end = rest[start + end_rel..]
                    .find('\n')
                    .map(|x| start + end_rel + x)
                    .unwrap_or(rest.len());
                out.push_str("<redacted:private-key>");
                rest = &rest[block_end..];
            } else {
                out.push_str("<redacted:private-key>");
                break;
            }
            continue;
        }
        // 2. Named key=value secrets (case-insensitive key).
        if let Some((key_start, key)) = find_named_key(rest) {
            out.push_str(&rest[..key_start]);
            out.push_str(key);
            out.push_str("<redacted>");
            let val_start = key_start + key.len();
            let val_end = rest[val_start..]
                .find(|c: char| c.is_whitespace() || c == '&' || c == '"' || c == '\'')
                .map(|x| val_start + x)
                .unwrap_or(rest.len());
            rest = &rest[val_end..];
            continue;
        }
        // 3. Platform token prefixes (case-insensitive).
        if let Some((prefix_start, prefix)) = find_token_prefix(rest) {
            out.push_str(&rest[..prefix_start]);
            out.push_str(prefix);
            out.push_str("<redacted>");
            let val_start = prefix_start + prefix.len();
            let val_end = rest[val_start..]
                .find(|c: char| {
                    !c.is_alphanumeric() && c != '-' && c != '_' && c != '.' && c != '/'
                })
                .map(|x| val_start + x)
                .unwrap_or(rest.len());
            rest = &rest[val_end..];
            continue;
        }
        // 4. URL userinfo: scheme://user:pass@host
        if let Some((ui_start, ui_len)) = find_url_userinfo(rest) {
            out.push_str(&rest[..ui_start]);
            let userinfo = &rest[ui_start..ui_start + ui_len];
            let user = userinfo.split_once(':').map(|(u, _)| u).unwrap_or(userinfo);
            out.push_str(user);
            out.push_str(":<redacted>@");
            rest = &rest[ui_start + ui_len..];
            continue;
        }
        // Nothing matched: copy one character and advance.
        let ch = rest.chars().next().unwrap();
        out.push(ch);
        rest = &rest[ch.len_utf8()..];
    }
    redact_bearer(&out)
}

/// Locate the first `key=` secret, matching case-insensitively. Returns
/// (byte offset of the key, the key with its original case).
fn find_named_key(rest: &str) -> Option<(usize, &str)> {
    let lower = rest.to_ascii_lowercase();
    for key in NAMED_KEYS {
        if let Some(pos) = lower.find(key) {
            let key_len = key.len();
            return Some((pos, &rest[pos..pos + key_len]));
        }
    }
    None
}

/// Locate the first platform token prefix at a word boundary,
/// case-insensitively.
fn find_token_prefix(rest: &str) -> Option<(usize, &str)> {
    let lower = rest.to_ascii_lowercase();
    for prefix in TOKEN_PREFIXES {
        if let Some(pos) = lower.find(prefix) {
            let boundary = match rest[..pos].chars().next_back() {
                None => true,
                Some(c) => !c.is_alphanumeric() && c != '_' && c != '-',
            };
            if !boundary {
                continue;
            }
            let prefix_len = prefix.len();
            return Some((pos, &rest[pos..pos + prefix_len]));
        }
    }
    None
}

/// Locate `user:password@` userinfo that follows a `://` scheme, returning
/// the byte offset of the userinfo start and its length.
fn find_url_userinfo(rest: &str) -> Option<(usize, usize)> {
    if let Some(scheme_end) = rest.find("://") {
        let after = &rest[scheme_end + 3..];
        if let Some(at) = after.find('@') {
            let userinfo = &after[..at];
            if userinfo.contains(':') {
                return Some((scheme_end + 3, at + 1));
            }
        }
    }
    None
}

const NAMED_KEYS: &[&str] = &[
    "password=",
    "passwd=",
    "pwd=",
    "token=",
    "access_token=",
    "refresh_token=",
    "api_key=",
    "apikey=",
    "api-key=",
    "secret=",
    "client_secret=",
    "private_key=",
    "clientsecret=",
    "authorization=",
    "authorization:",
];

const TOKEN_PREFIXES: &[&str] = &[
    "sk-",
    "ghp_",
    "gho_",
    "github_pat_",
    "glpat-",
    "xoxb-",
    "xoxp-",
    "xoxa-",
    "akia",
    "asia",
];

/// Mask `Bearer <token>` / `Basic <base64>` authorization payloads.
fn redact_bearer(s: &str) -> String {
    let mut out = s.to_string();
    for keyword in ["Bearer ", "Basic ", "bearer ", "basic "] {
        if let Some(pos) = out.find(keyword) {
            let rest = &out[pos + keyword.len()..];
            let end = rest
                .find(|c: char| c.is_whitespace() || c == '&' || c == '"' || c == '\'')
                .unwrap_or(rest.len());
            let tail = &rest[end..];
            out = format!("{}{}{}{}", &out[..pos], keyword, "<redacted>", tail);
            break;
        }
    }
    out
}

/// Redact userinfo inside URLs: `https://user:secret@host` → `https://user:<redacted>@host`.
/// Preserves the username, masks only the password portion.
pub fn redact_url_userinfo(s: &str) -> String {
    let mut out = s.to_string();
    if let Some(scheme_end) = out.find("://") {
        let after = &out[scheme_end + 3..];
        if let Some(at) = after.find('@') {
            let userinfo = &after[..at];
            if let Some(colon) = userinfo.find(':') {
                let user = &userinfo[..colon];
                out = format!(
                    "{}{}{}@{}",
                    &out[..scheme_end + 3],
                    user,
                    ":<redacted>",
                    &after[at + 1..]
                );
            }
        }
    }
    out
}

/// Redact a full free-form value, returning a bounded string. Used when a
/// caller-supplied value must be echoed back (e.g. a URL in an error).
pub fn redact_bounded(s: &str, max_chars: usize) -> String {
    let redacted = redact_value(s);
    crate::utils::truncate(&redacted, max_chars)
}

/// Scan a manifest/JSON value tree and redact any string leaves that carry
/// secret-looking values (deep redaction for structured data).
pub fn redact_json(value: &mut serde_json::Value) {
    match value {
        serde_json::Value::String(s) => {
            if looks_secretive(s) {
                *s = redact_value(s);
            }
        }
        serde_json::Value::Array(items) => {
            for item in items.iter_mut() {
                redact_json(item);
            }
        }
        serde_json::Value::Object(map) => {
            for (key, val) in map.iter_mut() {
                let key_lower = key.to_ascii_lowercase();
                let secretish_key = SECRET_KEYS.iter().any(|k| key_lower.contains(k));
                if secretish_key {
                    *val = serde_json::Value::String("<redacted>".to_string());
                } else {
                    redact_json(val);
                }
            }
        }
        _ => {}
    }
}

const SECRET_KEYS: &[&str] = &[
    "password",
    "passwd",
    "secret",
    "token",
    "api_key",
    "apikey",
    "access_key",
    "private_key",
    "client_secret",
    "authorization",
    "cookie",
    "cookie_header",
];

/// Does this string look like it carries a secret? Cheap heuristic used to
/// decide whether to redact before echoing.
fn looks_secretive(s: &str) -> bool {
    let lower = s.to_ascii_lowercase();
    SECRET_PATTERNS
        .iter()
        .any(|p| lower.contains(p) || s.contains(*p))
}

const SECRET_PATTERNS: &[&str] = &[
    "password=",
    "passwd=",
    "token=",
    "api_key=",
    "apikey=",
    "secret=",
    "client_secret=",
    "authorization=",
    "bearer ",
    "-----begin ",
    "sk-",
    "ghp_",
    "glpat-",
    "xoxb-",
    "://",
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redacts_common_token_formats() {
        assert!(redact_value("key=sk-1234567890abcdef").contains("sk-<redacted>"));
        assert!(!redact_value("sk-1234567890abcdef").contains("1234567890"));
        assert!(redact_value("ghp_abcdefghij").contains("ghp_<redacted>"));
        assert!(redact_value("glpat-xyz-12345").contains("glpat-<redacted>"));
    }

    #[test]
    fn redacts_named_secret_parameters() {
        let out = redact_value("--db password=supersecret --host x");
        assert!(out.contains("password=<redacted>"));
        assert!(!out.contains("supersecret"));
        assert!(redact_value("token=abc123").contains("token=<redacted>"));
        assert!(redact_value("client_secret=xyz").contains("client_secret=<redacted>"));
    }

    #[test]
    fn redacts_private_key_blocks() {
        let s = "-----BEGIN RSA PRIVATE KEY-----\nMIIE";
        let out = redact_value(s);
        assert!(out.contains("<redacted:private-key>"));
        assert!(!out.contains("MIIE"));
    }

    #[test]
    fn redacts_bearer_and_basic() {
        assert!(redact_value("Authorization: Bearer abcdef").contains("Bearer <redacted>"));
        assert!(!redact_value("Authorization: Bearer abcdef").contains("abcdef"));
        assert!(redact_value("Authorization: Basic dXNlcjpwYXNz").contains("Basic <redacted>"));
    }

    #[test]
    fn redacts_url_userinfo_but_keeps_username() {
        let out = redact_url_userinfo("http://alice:hunter2@localhost:3000/app");
        assert_eq!(out, "http://alice:<redacted>@localhost:3000/app");
        let no_creds = redact_url_userinfo("http://localhost:3000/app");
        assert_eq!(no_creds, "http://localhost:3000/app");
    }

    #[test]
    fn redact_json_masks_secret_keys_recursively() {
        let mut v = serde_json::json!({
            "name": "app",
            "config": { "password": "hunter2", "port": 3000 },
            "deps": ["lodash", {"token": "abc"}],
        });
        redact_json(&mut v);
        assert_eq!(v["config"]["password"], "<redacted>");
        assert_eq!(v["deps"][1]["token"], "<redacted>");
        assert_eq!(v["name"], "app");
    }

    #[test]
    fn redact_bounded_truncates() {
        let out = redact_bounded(&"x".repeat(500), 50);
        assert!(out.chars().count() <= 51);
    }
}
