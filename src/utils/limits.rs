//! Resource-limit helpers.
//!
//! AI agents can request broad information, so every tool bounds its output
//! and every payload is capped before serialization.

use crate::errors::WinkitError;

/// Serialize `value` and fail with [`ErrorKind::ResourceLimit`] if the
/// encoded payload exceeds `max_bytes`. This keeps MCP responses bounded
/// even when a provider over-delivers.
pub fn encode_limited(value: &serde_json::Value, max_bytes: usize) -> Result<Vec<u8>, WinkitError> {
    let bytes = serde_json::to_vec(value)?;
    if bytes.len() > max_bytes {
        return Err(WinkitError::resource_limit(format!(
            "response payload of {} bytes exceeds the configured limit of {max_bytes} bytes",
            bytes.len()
        )));
    }
    Ok(bytes)
}

/// Truncate a slice to `limit` items.
pub fn truncate<T>(items: Vec<T>, limit: usize) -> Vec<T> {
    if items.len() <= limit {
        items
    } else {
        items.into_iter().take(limit).collect()
    }
}

/// Clamp `value` into `[min, max]`.
pub fn clamp_u64(value: u64, min: u64, max: u64) -> u64 {
    value.clamp(min, max)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::errors::ErrorKind;

    #[test]
    fn encode_limited_rejects_oversized_payloads() {
        let big = serde_json::json!({ "data": vec!["x"; 10_000] });
        let err = encode_limited(&big, 100).unwrap_err();
        assert_eq!(err.kind, ErrorKind::ResourceLimit);
    }

    #[test]
    fn encode_limited_accepts_small_payloads() {
        let small = serde_json::json!({ "ok": true });
        let bytes = encode_limited(&small, 1024).unwrap();
        assert!(!bytes.is_empty());
    }

    #[test]
    fn truncate_bounds_output() {
        let v = vec![1, 2, 3, 4, 5];
        assert_eq!(truncate(v, 2), vec![1, 2]);
    }

    #[test]
    fn clamp_u64_bounds() {
        assert_eq!(clamp_u64(5, 10, 20), 10);
        assert_eq!(clamp_u64(15, 10, 20), 15);
        assert_eq!(clamp_u64(25, 10, 20), 20);
    }
}
