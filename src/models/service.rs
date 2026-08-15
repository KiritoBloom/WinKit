//! Service observability models.

use serde::{Deserialize, Serialize};

/// A Windows service, read-only view.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ServiceInfo {
    pub name: String,
    pub display_name: String,
    /// Current state: `running`, `stopped`, `start_pending`, `stop_pending`,
    /// `pause_pending`, `continue_pending`, `paused`, `unknown`.
    pub state: String,
    /// Service type, e.g. `win32`, `win32_own_process`, `win32_share_process`,
    /// `kernel`, `driver`.
    pub service_type: String,
    pub process_id: Option<u32>,
    pub win32_exit_code: Option<u32>,
    /// Start type (`auto`, `manual`, `disabled`, `boot`, `system`) — only in
    /// `get_service` results.
    pub start_type: Option<String>,
    /// Binary path — only in `get_service` results.
    pub binary_path: Option<String>,
    /// Account the service runs under — only in `get_service` results.
    pub service_start_name: Option<String>,
}
