//! Disk-scan models: the reusable per-volume snapshot surface exposed to
//! MCP tools.
//!
//! The heavy in-memory representation lives in
//! [`crate::platform::windows::diskscan`]; these models are the compact,
//! serializable view of a snapshot and the request types the tool layer
//! passes to the backend.

use serde::Serialize;

/// Which scanner implementation produced a snapshot. The MCP never pretends
/// the fast path was used when it was not: the scanner kind is always
/// surfaced in results.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ScannerKind {
    /// NTFS MFT enumeration via `FSCTL_ENUM_USN_DATA` (fast path).
    NtfsMft,
    /// Generic recursive directory walk (non-NTFS or fast path unavailable).
    RecursiveFallback,
}

impl ScannerKind {
    pub fn as_str(self) -> &'static str {
        match self {
            ScannerKind::NtfsMft => "ntfs_mft_fast",
            ScannerKind::RecursiveFallback => "recursive_fallback",
        }
    }
}

/// One indexed file, fully materialized (path resolved).
#[derive(Debug, Clone, Serialize)]
pub struct ScanFileEntry {
    pub path: String,
    /// Logical (EndOfFile) size in bytes — the primary size metric.
    pub size_bytes: u64,
    /// On-disk allocated size in bytes. Only present when it was actually
    /// measured (reparse points, or targeted top-K queries). Never faked.
    pub allocated_bytes: Option<u64>,
    /// Number of directory entries (hard links) referring to this file.
    pub hard_links: u64,
    pub is_reparse_point: bool,
    /// RFC3339 last-write time, when available.
    pub modified: Option<String>,
}

/// One indexed directory with its aggregate size.
#[derive(Debug, Clone, Serialize)]
pub struct ScanFolderEntry {
    pub path: String,
    /// Aggregate logical size of all descendant files (files only, as in
    /// Windows Explorer; directory metadata overhead is not counted).
    pub size_bytes: u64,
    pub files: u64,
    pub directories: u64,
}

/// Capacity numbers for the scanned volume.
#[derive(Debug, Clone, Serialize)]
pub struct ScanCapacity {
    pub total_bytes: Option<u64>,
    pub free_bytes: Option<u64>,
    pub used_bytes: Option<u64>,
}

/// The one-call overview returned by `disk_scan` (and embedded in the
/// completed background-scan status). Compact by design: top lists default
/// to small limits so the MCP response stays small.
#[derive(Debug, Clone, Serialize)]
pub struct DiskScanInfo {
    pub volume: String,
    pub filesystem: String,
    pub scanner: String,
    /// Set when the fast path was unavailable and the scan fell back.
    pub fast_path_unavailable: Option<String>,
    pub cached: bool,
    pub cache_age_ms: Option<u64>,
    pub scan_duration_ms: u64,
    pub scanned_at: Option<String>,
    pub files_indexed: u64,
    pub directories_indexed: u64,
    /// Directory entries that are additional hard links (same file ID as
    /// another entry). Their sizes are counted once per link.
    pub hard_links: u64,
    pub reparse_points: u64,
    /// Records whose parent could not be resolved; attached to the root.
    pub orphans: u64,
    /// Files whose size could not be read (e.g. access denied); excluded
    /// from aggregates.
    pub size_unknown: u64,
    /// MFT records that disappeared between enumeration and the size pass
    /// (deleted/stale); dropped from the snapshot.
    pub stale_records_dropped: u64,
    /// Records dropped as duplicate names (e.g. 8.3 short names) for the
    /// same (file, parent) pair.
    pub duplicate_names_dropped: u64,
    pub total_logical_bytes: u64,
    pub capacity: Option<ScanCapacity>,
    pub largest_files: Vec<ScanFileEntry>,
    pub largest_folders: Vec<ScanFolderEntry>,
}

/// Status of a background scan.
#[derive(Debug, Clone, Serialize)]
pub struct DiskScanStatusInfo {
    pub scan_id: String,
    pub volume: String,
    /// `enumeration`, `size_pass`, `indexing`, `done`, `cancelled`, `failed`.
    pub phase: String,
    pub records_so_far: u64,
    pub files_so_far: u64,
    pub directories_so_far: u64,
    pub elapsed_ms: u64,
    /// 0-100 percent complete when a total can be estimated; None otherwise
    /// (e.g. a recursive fallback walk with no known total).
    pub progress_percent: Option<f64>,
    /// Estimated seconds remaining when progress is known and < 100; None
    /// otherwise.
    pub eta_seconds: Option<u64>,
    pub done: bool,
    pub cancelled: bool,
    pub error: Option<String>,
    /// Present when the scan completed successfully.
    pub result: Option<DiskScanInfo>,
}

/// One folder-size result.
#[derive(Debug, Clone, Serialize)]
pub struct ScanFolderSize {
    pub path: String,
    pub size_bytes: u64,
    pub files: u64,
    pub directories: u64,
}

/// A file found by `disk_scan_find`.
#[derive(Debug, Clone, Serialize)]
pub struct ScanFindFile {
    pub path: String,
    pub size_bytes: u64,
    pub is_reparse_point: bool,
    pub modified: Option<String>,
}

/// Typed result of a snapshot query. Tools serialize these directly.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum DiskQueryResult {
    TopFiles {
        entries: Vec<ScanFileEntry>,
        /// Diagnostics: how the answer was produced.
        volume: String,
        scanner: String,
        /// Set when the fast path was unavailable and the scan fell back;
        /// carried verbatim from the snapshot so query tools name the exact
        /// reason like `disk_scan` does.
        fast_path_unavailable: Option<String>,
        cached: bool,
        snapshot_age_ms: Option<u64>,
    },
    TopFolders {
        entries: Vec<ScanFolderEntry>,
        volume: String,
        scanner: String,
        fast_path_unavailable: Option<String>,
        cached: bool,
        snapshot_age_ms: Option<u64>,
    },
    FolderSize {
        folder: ScanFolderSize,
        volume: String,
        scanner: String,
        fast_path_unavailable: Option<String>,
        cached: bool,
        snapshot_age_ms: Option<u64>,
    },
    FindFiles {
        entries: Vec<ScanFindFile>,
        truncated: bool,
        volume: String,
        scanner: String,
        fast_path_unavailable: Option<String>,
        cached: bool,
        snapshot_age_ms: Option<u64>,
    },
}

/// Which query to run against a snapshot.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiskQueryKind {
    TopFiles,
    TopFolders,
    FolderSize,
    FindFiles,
}

/// A query against the cached snapshot of the volume containing `path`.
#[derive(Debug, Clone)]
pub struct DiskQueryRequest {
    /// Any path on the target volume; the volume root is derived from it.
    /// For subtree-limited queries this is also the subtree root.
    pub path: String,
    pub kind: DiskQueryKind,
    pub limit: usize,
    pub min_size_bytes: u64,
    /// Optional extension filter (case-insensitive, no leading dot).
    pub extensions: Option<Vec<String>>,
    /// Optional case-insensitive name substring filter for `FindFiles`.
    pub pattern: Option<String>,
}

/// Parameters for a full scan of the volume containing `path`.
#[derive(Debug, Clone)]
pub struct DiskScanRequest {
    /// Any path on the target volume (e.g. `D:`, `D:\`, `D:\Games`).
    pub path: String,
    /// Force a rescan even when a fresh-enough snapshot is cached.
    pub refresh: bool,
    /// Cache freshness threshold in ms. A snapshot younger than this is
    /// served without rescanning. Default (0 is treated as) 30_000.
    pub max_age_ms: u64,
}
