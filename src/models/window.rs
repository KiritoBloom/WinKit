//! Window observability models.

use serde::{Deserialize, Serialize};

/// A top-level window, read-only view.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct WindowInfo {
    /// Win32 HWND value (as integer).
    pub hwnd: isize,
    pub title: String,
    pub class_name: Option<String>,
    pub process_id: u32,
    /// Process name resolved from the owning PID, when available.
    pub process_name: Option<String>,
    pub visible: bool,
    pub minimized: bool,
    pub maximized: bool,
    /// True if this is the foreground (active) window.
    pub foreground: bool,
}
