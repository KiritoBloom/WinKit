//! Small shared utilities: logging, time helpers, wide-string helpers,
//! resource-limit helpers, a minimal HTTP client used for Chrome DevTools
//! endpoint discovery, plus secret redaction, local-URL validation, and the
//! bounded web-app probe used by `diagnose_local_webapp`.

pub mod blocking;
pub mod http;
pub mod limits;
pub mod log;
pub mod redact;
pub mod time;
pub mod url;
pub mod wait;
pub mod webapp;
pub mod workspace;

use std::ffi::CStr;

/// Convert a UTF-8 string into a NUL-terminated UTF-16 buffer for Win32 calls.
pub fn to_wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// Convert a NUL-terminated UTF-16 slice into a `String`, tolerating
/// invalid surrogate pairs.
pub fn wide_to_string(wide: &[u16]) -> String {
    let len = wide.iter().position(|&c| c == 0).unwrap_or(wide.len());
    String::from_utf16_lossy(&wide[..len])
}

/// Convert a fixed-size NUL-terminated byte array (e.g. a Win32 fixed
/// char buffer) into a trimmed `String`.
pub fn fixed_bytes_to_string<const N: usize>(bytes: &[u8; N]) -> String {
    let end = bytes.iter().position(|&b| b == 0).unwrap_or(N);
    String::from_utf8_lossy(&bytes[..end]).trim().to_string()
}

/// Convert a `*const i8` (Win32 `c_char`) into a `String`, or an empty
/// string on failure.
///
/// # Safety
///
/// `ptr` must be null or point to a valid NUL-terminated string that stays
/// alive for the duration of the call.
pub unsafe fn cstr_ptr_to_string(ptr: *const i8) -> String {
    if ptr.is_null() {
        return String::new();
    }
    CStr::from_ptr(ptr).to_string_lossy().into_owned()
}

/// Truncate a string to at most `max` chars, appending an ellipsis when
/// truncation occurred. Used to keep unbounded user/URL text out of payloads.
pub fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let cut: String = s.chars().take(max.saturating_sub(1)).collect();
        format!("{cut}…")
    }
}
