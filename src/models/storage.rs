//! Storage observability models.

use serde::{Deserialize, Serialize};

/// A drive/volume as reported by `GetLogicalDrives`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct DriveInfo {
    /// Drive letter with trailing separator, e.g. `C:\`.
    pub root: String,
    /// Drive type, e.g. `fixed`, `removable`, `remote`, `cdrom`.
    pub kind: String,
    pub total_bytes: Option<u64>,
    pub free_bytes: Option<u64>,
    pub used_bytes: Option<u64>,
    pub percent_used: Option<f64>,
}

/// Volume usage for a specific path.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct DiskUsage {
    pub path: String,
    pub total_bytes: Option<u64>,
    pub free_bytes: Option<u64>,
    pub used_bytes: Option<u64>,
    pub percent_used: Option<f64>,
}
