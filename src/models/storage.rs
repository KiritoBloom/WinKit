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

/// A file found by `find_large_files`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct FileEntry {
    pub path: String,
    pub size_bytes: u64,
    /// RFC3339 modified time, when available.
    pub modified: Option<String>,
}

/// Parameters for the large-file scan. `path` is mandatory; the tool
/// refuses to scan a whole drive implicitly.
#[derive(Debug, Clone)]
pub struct FindLargeFilesRequest {
    pub path: std::path::PathBuf,
    pub min_size_bytes: u64,
    pub max_depth: u32,
    pub max_results: usize,
    pub extensions: Option<Vec<String>>,
}
