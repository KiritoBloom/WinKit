//! Fast disk-space analysis for WinKit.
//!
//! # Architecture
//!
//! ```text
//! user requests disk analysis
//!         │
//!         ▼
//! identify volume + filesystem          (plan_for_path)
//!         │
//!         ▼
//! choose scanner                        (ScannerPlan.kind)
//!         │
//!         ├─ NTFS ──► FSCTL_ENUM_USN_DATA (ntfs.rs) ──► size pass (sizes.rs)
//!         │                                                      │
//!         └─ other ─► recursive fallback (fallback.rs) ◄─────────┘
//!                                │
//!                                ▼
//!                    TreeIndex + aggregation (tree.rs)
//!                                │
//!                                ▼
//!                       DiskSnapshot (cached, queryable)
//! ```
//!
//! Scans are cached per volume root (`C:\`), served synchronously by
//! [`DiskScanService::sync_scan`], or run in the background with progress
//! and cancellation ([`DiskScanService::start`] / [`status`] / [`cancel`]).
//!
//! The MCP never pretends the fast path was used when it was not: every
//! result carries the scanner kind, and a failed fast path falls back to the
//! recursive scanner with an explicit `fast_path_unavailable` reason.
//!
//! # Size semantics
//!
//! The primary metric is **logical size** (`EndOfFile`), matching Windows
//! Explorer. On-disk/allocated size is only measured for materialized
//! top-K results (a handful of targeted opens). Reparse points are never
//! followed (no cycles, no double counting, no volume escapes); hard links
//! are counted once per directory entry, matching Explorer, and are exposed
//! with their link count.

pub mod fallback;
pub mod ntfs;
pub mod sizes;
pub mod tree;

use crate::errors::{ErrorKind, WinkitError};
use crate::models::{
    DiskQueryResult, DiskScanInfo, DiskScanRequest, DiskScanStatusInfo, ScanCapacity,
    ScanFileEntry, ScanFindFile, ScanFolderEntry, ScanFolderSize, ScannerKind,
};
use crate::platform::windows::storage::volume_usage;
use crate::utils::time::format_rfc3339;
use ntfs::{
    FLAG_DIRECTORY, FLAG_EXTRA_LINK, FLAG_ORPHANED, FLAG_REPARSE, FLAG_SIZE_UNKNOWN, FLAG_STALE,
};
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Instant, SystemTime};
use tree::{PathResolver, TopK};

/// Default cache freshness: a snapshot younger than this is served without
/// a rescan.
const DEFAULT_CACHE_TTL_MS: u64 = 30_000;

// Volume identification and scanner selection

/// Normalize a user-supplied path for drive-relative, rooted, and relative
/// inputs (`D:`, `D:\`, `d:\Games`, `D:\Games\Steam`, `.`, `..\x`).
pub fn normalize_path(path: &str) -> Result<String, WinkitError> {
    let p = path.trim();
    if p.is_empty() {
        return Err(WinkitError::invalid_argument("path must not be empty"));
    }
    let bytes = p.as_bytes();
    if bytes.len() >= 2 && bytes[1] == b':' && bytes[0].is_ascii_alphabetic() {
        let letter = bytes[0].to_ascii_uppercase() as char;
        let rest = p[2..].trim_start_matches(['\\', '/']);
        if rest.is_empty() {
            return Ok(format!("{letter}:\\"));
        }
        Ok(format!("{letter}:\\{rest}"))
    } else if p.starts_with("\\\\") {
        Ok(p.replace('/', "\\"))
    } else {
        let cwd = std::env::current_dir().map_err(|e| {
            WinkitError::invalid_argument(format!("cannot resolve relative path '{p}': {e}"))
        })?;
        normalize_path(&cwd.join(p).to_string_lossy())
    }
}

/// The volume root for a normalized path (`D:\Games` → `D:\`,
/// `\\srv\share\x` → `\\srv\share`).
pub fn volume_root_of(path: &str) -> String {
    let b = path.as_bytes();
    if b.len() >= 3 && b[1] == b':' && b[2] == b'\\' {
        format!("{}:\\", path[..1].to_ascii_uppercase())
    } else if path.starts_with("\\\\") {
        let parts: Vec<&str> = path.trim_start_matches('\\').split('\\').collect();
        if parts.len() >= 2 {
            format!("\\\\{}\\{}", parts[0], parts[1])
        } else {
            path.to_string()
        }
    } else {
        path.to_string()
    }
}

/// Which scanner a volume should use, decided automatically at runtime.
#[derive(Debug, Clone)]
pub struct ScannerPlan {
    pub volume_root: String,
    /// The directory actually walked by the recursive fallback. Equals the
    /// volume root when the fast path is available (the MFT covers the whole
    /// volume cheaply) or when the caller asked about a drive root. When the
    /// fast path is unavailable and the caller named a subdirectory, the
    /// fallback walks only that subtree — bounded, and the same path the
    /// query tools answer against.
    pub scope_root: String,
    pub filesystem: String,
    pub kind: ScannerKind,
}

/// Is `path` (a normalized absolute path) inside or equal to `base`?
/// Component-aware: `D:\GamesX` is not under `D:\Games`, but `D:\Games\Sub`
/// is, and anything after a root like `D:\` is a descendant.
fn path_under(base: &str, path: &str) -> bool {
    if path == base {
        return true;
    }
    match path.strip_prefix(base) {
        Some(rest) => {
            if base.ends_with('\\') {
                !rest.is_empty()
            } else {
                rest.starts_with('\\')
            }
        }
        None => false,
    }
}

/// The directory a recursive fallback walk of `normalized` should cover:
/// the path itself when it exists as a directory, else the volume root.
fn fallback_scope(normalized: &str) -> String {
    if std::fs::metadata(normalized)
        .map(|m| m.is_dir())
        .unwrap_or(false)
    {
        normalized.to_string()
    } else {
        volume_root_of(normalized)
    }
}

/// Identify the volume and choose a scanner for `path`. The MCP never asks
/// the agent to pick "NTFS mode" — this decision is made here.
pub fn plan_for_path(path: &str) -> Result<ScannerPlan, WinkitError> {
    let normalized = normalize_path(path)?;
    let root = volume_root_of(&normalized);
    let fs = ntfs::filesystem_name(&root)?;
    let kind = if fs.eq_ignore_ascii_case("NTFS") {
        ScannerKind::NtfsMft
    } else {
        ScannerKind::RecursiveFallback
    };
    Ok(ScannerPlan {
        volume_root: root,
        scope_root: fallback_scope(&normalized),
        filesystem: fs,
        kind,
    })
}

fn capacity_for(root: &str) -> Option<ScanCapacity> {
    volume_usage(root).map(|(total, free, _)| ScanCapacity {
        total_bytes: Some(total),
        free_bytes: Some(free),
        used_bytes: Some(total.saturating_sub(free)),
    })
}

// Core records

/// One enumerated directory entry. Compact by design: identity is the file
/// reference number (`frn`), never a path string; names live in a shared
/// arena referenced by `(name_off, name_len)`.
#[derive(Debug, Clone, Copy)]
pub struct ScanRecord {
    pub frn: u64,
    pub parent_frn: u64,
    /// Logical size (EndOfFile); 0 for directories.
    pub size: u64,
    /// Last-write time, unix seconds (0 when unknown).
    pub mtime: i64,
    /// Offset into the snapshot's name arena.
    pub name_off: u32,
    pub name_len: u16,
    pub attributes: u32,
    pub flags: u8,
}

/// Honest aggregate counters for a snapshot.
#[derive(Debug, Clone, Copy, Default)]
pub struct ScanCounts {
    pub files: u64,
    pub dirs: u64,
    /// Extra directory entries that are hard links to the same file.
    pub hard_links: u64,
    pub reparse: u64,
    /// Records whose parent could not be resolved (attached to the root).
    pub orphans: u64,
    pub size_unknown: u64,
    /// MFT records whose file disappeared before the size pass.
    pub stale_dropped: u64,
    /// Records dropped as duplicate names (e.g. 8.3 short names) for the
    /// same (file, parent) pair.
    pub duplicate_names: u64,
    /// Sum of all file logical sizes (the root folder size).
    pub total_logical: u64,
}

/// Phase timings of a scan.
#[derive(Debug, Clone, Copy, Default)]
pub struct ScanTimings {
    pub enum_ms: u64,
    pub size_ms: u64,
    pub index_ms: u64,
    pub total_ms: u64,
}

/// Progress of a background scan, shared with the spawned worker.
#[derive(Debug)]
pub struct ScanProgress {
    phase: Mutex<String>,
    records: AtomicU64,
    files: AtomicU64,
    dirs: AtomicU64,
    /// Denominator for percent progress: 0 means no total is known and
    /// progress must stay `None` (honest — never a guess).
    total_records: AtomicU64,
}

impl Default for ScanProgress {
    fn default() -> Self {
        Self::new()
    }
}

impl ScanProgress {
    pub fn new() -> Self {
        Self {
            phase: Mutex::new("starting".to_string()),
            records: AtomicU64::new(0),
            files: AtomicU64::new(0),
            dirs: AtomicU64::new(0),
            total_records: AtomicU64::new(0),
        }
    }
    pub fn set_phase(&self, phase: &str) {
        *self.phase.lock().unwrap() = phase.to_string();
    }
    pub fn phase(&self) -> String {
        self.phase.lock().unwrap().clone()
    }
    pub fn set_records(&self, n: u64) {
        self.records.store(n, Ordering::Relaxed);
    }
    pub fn records(&self) -> u64 {
        self.records.load(Ordering::Relaxed)
    }
    pub fn set_files(&self, n: u64) {
        self.files.store(n, Ordering::Relaxed);
    }
    pub fn files(&self) -> u64 {
        self.files.load(Ordering::Relaxed)
    }
    pub fn set_dirs(&self, n: u64) {
        self.dirs.store(n, Ordering::Relaxed);
    }
    pub fn dirs(&self) -> u64 {
        self.dirs.load(Ordering::Relaxed)
    }
    pub fn set_total_records(&self, n: u64) {
        self.total_records.store(n, Ordering::Relaxed);
    }
    pub fn total_records(&self) -> u64 {
        self.total_records.load(Ordering::Relaxed)
    }
    /// 0-100 percent complete when a total is known; `None` when the total
    /// is unknown (e.g. a recursive fallback walk). Never a guess.
    pub fn progress_percent(&self) -> Option<f64> {
        let total = self.total_records();
        if total == 0 {
            return None;
        }
        let records = self.records();
        Some((records as f64 / total as f64 * 100.0).clamp(0.0, 100.0))
    }
    /// Estimated seconds remaining at the current rate, given the elapsed
    /// time. `None` while progress is zero or unknown; `Some(0)` at 100%.
    pub fn eta_seconds(&self, elapsed_ms: u64) -> Option<u64> {
        let pct = self.progress_percent()?;
        if pct <= 0.0 {
            return None;
        }
        let remaining = (100.0 - pct) / pct * elapsed_ms as f64 / 1000.0;
        Some(remaining.ceil() as u64)
    }
}

// The snapshot

/// A complete, queryable scan of one volume. Built once, cached, then
/// queried many times in milliseconds.
#[derive(Debug)]
pub struct DiskSnapshot {
    pub volume_root: String,
    /// The directory the snapshot's records actually cover. Equals the
    /// volume root for the NTFS fast path; for a scoped recursive fallback
    /// it is the requested directory (its synthetic root record). Full
    /// paths and path resolution are relative to this base.
    pub scope_root: String,
    pub filesystem: String,
    pub scanner: ScannerKind,
    pub fast_path_unavailable: Option<String>,
    pub records: Vec<ScanRecord>,
    pub names: Vec<u8>,
    pub root_frn: u64,
    pub scanned_at: SystemTime,
    pub timings: ScanTimings,
    pub counts: ScanCounts,
    pub capacity: Option<ScanCapacity>,
    pub tree: tree::TreeIndex,
    /// Precomputed whole-volume top lists (size, index), so repeated
    /// "biggest on D:" queries do no work at all.
    pub top_files_all: Vec<(u64, u32)>,
    pub top_folders_all: Vec<(u64, u32)>,
}

impl DiskSnapshot {
    fn resolver(&self) -> PathResolver<'_> {
        PathResolver {
            records: &self.records,
            names: &self.names,
            by_frn: &self.tree.by_frn,
            root_frn: self.root_frn,
            // Paths are rebuilt from the scope root (the synthetic root
            // record of a scoped fallback has an empty name, so the
            // volume root alone would produce paths missing the scope
            // component). For whole-volume snapshots scope == volume.
            volume_root: &self.scope_root,
        }
    }

    pub fn name_of(&self, idx: u32) -> &str {
        self.resolver().name_of(idx)
    }

    pub fn path_of(&self, idx: u32) -> String {
        self.resolver()
            .path_of(idx)
            .unwrap_or_else(|| format!("{}<unresolved>", self.volume_root))
    }

    /// Index of the volume root record.
    pub fn root_index(&self) -> Option<u32> {
        self.resolver().lookup(self.root_frn)
    }

    /// Resolve a normalized path to a record index (root, or a descendant).
    /// Paths are matched case-insensitively, component by component.
    pub fn resolve_path(&self, path: &str) -> Result<u32, WinkitError> {
        let normalized = normalize_path(path)?;
        // Relative to the scan scope first (the snapshot may only cover a
        // subtree when the recursive fallback was scoped); fall back to the
        // volume root so whole-volume paths keep resolving.
        let rel = if path_under(&self.scope_root, &normalized) {
            normalized
                .strip_prefix(&self.scope_root)
                .unwrap_or("")
                .trim_start_matches('\\')
        } else if path_under(&self.volume_root, &normalized) {
            normalized
                .strip_prefix(&self.volume_root)
                .unwrap_or("")
                .trim_start_matches('\\')
        } else {
            &normalized
        };
        let root = self
            .root_index()
            .ok_or_else(|| WinkitError::internal("volume root is missing from the snapshot"))?;
        let mut cur = root;
        for comp in rel
            .split(['\\', '/'])
            .filter(|s| !s.is_empty() && *s != ".")
        {
            if comp == ".." {
                return Err(WinkitError::invalid_argument(
                    "'..' is not supported in disk-scan paths",
                ));
            }
            let parent_frn = self.records[cur as usize].frn;
            let range = self.tree.children_range(&self.records, parent_frn);
            let found = self.tree.by_parent[range]
                .iter()
                .find(|&&c| self.name_of(c).eq_ignore_ascii_case(comp))
                .copied();
            match found {
                Some(c) => cur = c,
                None => {
                    return Err(WinkitError::not_found(format!(
                        "'{path}' is not present in the snapshot (snapshot is {} old; rescan with refresh=true if the path was created recently)",
                        snapshot_age_label(self.scanned_at)
                    )))
                }
            }
        }
        Ok(cur)
    }

    /// Call `f` for every record in the subtree rooted at `root` (inclusive),
    /// depth-first with an explicit stack.
    pub fn for_each_in_subtree(&self, root: u32, f: &mut impl FnMut(u32)) {
        let mut stack = vec![root];
        while let Some(d) = stack.pop() {
            f(d);
            let parent_frn = self.records[d as usize].frn;
            let range = self.tree.children_range(&self.records, parent_frn);
            for &c in self.tree.by_parent[range].iter().rev() {
                if c != d {
                    stack.push(c); // skip the root's self-parent reference
                }
            }
        }
    }

    /// Top-K largest files on the whole volume or under a subtree.
    pub fn top_files(
        &self,
        subtree: Option<u32>,
        limit: usize,
        min_size: u64,
        extensions: Option<&[String]>,
    ) -> Vec<ScanFileEntry> {
        // Whole-volume, unfiltered: serve from the precomputed list.
        if subtree.is_none() && min_size == 0 && extensions.is_none() {
            let mut out = Vec::with_capacity(limit.min(self.top_files_all.len()));
            for &(_, idx) in self.top_files_all.iter().take(limit) {
                out.push(self.file_entry(idx));
            }
            return out;
        }
        let mut top = TopK::new(limit);
        let visit = |i: u32, top: &mut TopK| {
            let r = &self.records[i as usize];
            if r.flags & FLAG_DIRECTORY != 0 || r.size < min_size {
                return;
            }
            if let Some(exts) = extensions {
                if !tree::extension_matches(self.name_of(i), exts) {
                    return;
                }
            }
            top.push(r.size, i);
        };
        match subtree {
            None => {
                for (i, r) in self.records.iter().enumerate() {
                    if r.flags & FLAG_DIRECTORY == 0 {
                        visit(i as u32, &mut top);
                    }
                }
            }
            Some(root) => {
                self.for_each_in_subtree(root, &mut |i| visit(i, &mut top));
            }
        }
        top.into_sorted()
            .into_iter()
            .map(|(_, idx)| self.file_entry(idx))
            .collect()
    }

    /// Top-K largest directories (descendants of `subtree`, or all
    /// directories on the volume). The subtree root itself is excluded.
    pub fn top_folders(&self, subtree: Option<u32>, limit: usize) -> Vec<ScanFolderEntry> {
        if subtree.is_none() {
            let mut out = Vec::with_capacity(limit.min(self.top_folders_all.len()));
            for &(_, idx) in self.top_folders_all.iter().take(limit) {
                out.push(self.folder_entry(idx));
            }
            return out;
        }
        let mut top = TopK::new(limit);
        let root = subtree.expect("subtree handled above");
        self.for_each_in_subtree(root, &mut |i| {
            let r = &self.records[i as usize];
            if r.flags & FLAG_DIRECTORY == 0 || i == root {
                return;
            }
            top.push(self.tree.aggregate[i as usize], i);
        });
        top.into_sorted()
            .into_iter()
            .map(|(_, idx)| self.folder_entry(idx))
            .collect()
    }

    /// Aggregate size of one directory from the in-memory index.
    pub fn folder_size(&self, idx: u32) -> ScanFolderSize {
        ScanFolderSize {
            path: self.path_of(idx),
            size_bytes: self.tree.aggregate[idx as usize],
            files: self.tree.dir_files[idx as usize],
            directories: self.tree.dir_dirs[idx as usize],
        }
    }

    /// Files matching `pattern` (wildcards `*`/`?`, else substring),
    /// extensions, and minimum size. Returns `(matches, truncated)` where
    /// `truncated` is true when more than `limit` files matched.
    pub fn find_files(
        &self,
        subtree: Option<u32>,
        pattern: Option<&str>,
        extensions: Option<&[String]>,
        min_size: u64,
        limit: usize,
    ) -> (Vec<ScanFindFile>, bool) {
        let mut top = TopK::new(limit);
        let mut matched: u64 = 0;
        let mut visit = |i: u32, top: &mut TopK| {
            let r = &self.records[i as usize];
            if r.flags & FLAG_DIRECTORY != 0 || r.size < min_size {
                return;
            }
            let name = self.name_of(i);
            if let Some(p) = pattern {
                if !tree::pattern_matches(name, p) {
                    return;
                }
            }
            if let Some(exts) = extensions {
                if !tree::extension_matches(name, exts) {
                    return;
                }
            }
            matched += 1;
            top.push(r.size, i);
        };
        match subtree {
            None => {
                for (i, r) in self.records.iter().enumerate() {
                    if r.flags & FLAG_DIRECTORY == 0 {
                        visit(i as u32, &mut top);
                    }
                }
            }
            Some(root) => {
                self.for_each_in_subtree(root, &mut |i| visit(i, &mut top));
            }
        }
        let entries = top
            .into_sorted()
            .into_iter()
            .map(|(_, idx)| {
                let r = &self.records[idx as usize];
                ScanFindFile {
                    path: self.path_of(idx),
                    size_bytes: r.size,
                    is_reparse_point: r.flags & FLAG_REPARSE != 0,
                    modified: mtime_to_rfc3339(r.mtime),
                }
            })
            .collect();
        (entries, matched > limit as u64)
    }

    /// Materialize one file entry. Allocated size and link count come from a
    /// targeted open of the path (only called for materialized results).
    fn file_entry(&self, idx: u32) -> ScanFileEntry {
        let r = &self.records[idx as usize];
        let path = self.path_of(idx);
        let (allocated, links) = sizes::allocated_and_links(&path)
            .map(|(a, l)| (Some(a), l as u64))
            .unwrap_or((None, 0));
        let hard_links = if links > 0 {
            links
        } else {
            // Fall back to counting records sharing this FRN.
            self.count_frn_entries(r.frn)
        };
        ScanFileEntry {
            path,
            size_bytes: r.size,
            allocated_bytes: allocated,
            hard_links: hard_links.max(1),
            is_reparse_point: r.flags & FLAG_REPARSE != 0,
            modified: mtime_to_rfc3339(r.mtime),
        }
    }

    fn folder_entry(&self, idx: u32) -> ScanFolderEntry {
        ScanFolderEntry {
            path: self.path_of(idx),
            size_bytes: self.tree.aggregate[idx as usize],
            files: self.tree.dir_files[idx as usize],
            directories: self.tree.dir_dirs[idx as usize],
        }
    }

    fn count_frn_entries(&self, frn: u64) -> u64 {
        let by_frn = &self.tree.by_frn;
        let lo = by_frn.partition_point(|&c| self.records[c as usize].frn < frn);
        let hi = by_frn.partition_point(|&c| self.records[c as usize].frn <= frn);
        (hi - lo) as u64
    }

    /// The compact, serializable overview used by `disk_scan` and
    /// background-scan status.
    pub fn info(&self, cached: bool, cache_age_ms: Option<u64>) -> DiskScanInfo {
        let top_files: Vec<ScanFileEntry> = self.top_files(None, 10, 0, None);
        let top_folders: Vec<ScanFolderEntry> = self.top_folders(None, 10);
        DiskScanInfo {
            volume: self.volume_root.clone(),
            filesystem: self.filesystem.clone(),
            scanner: self.scanner.as_str().to_string(),
            fast_path_unavailable: self.fast_path_unavailable.clone(),
            cached,
            cache_age_ms,
            scan_duration_ms: self.timings.total_ms,
            scanned_at: Some(format_rfc3339(self.scanned_at)),
            files_indexed: self.counts.files,
            directories_indexed: self.counts.dirs,
            hard_links: self.counts.hard_links,
            reparse_points: self.counts.reparse,
            orphans: self.counts.orphans,
            size_unknown: self.counts.size_unknown,
            stale_records_dropped: self.counts.stale_dropped,
            duplicate_names_dropped: self.counts.duplicate_names,
            total_logical_bytes: self.counts.total_logical,
            capacity: self.capacity.clone(),
            largest_files: top_files,
            largest_folders: top_folders,
        }
    }
}

fn mtime_to_rfc3339(mtime: i64) -> Option<String> {
    if mtime <= 0 {
        return None;
    }
    let st = std::time::UNIX_EPOCH + std::time::Duration::from_secs(mtime as u64);
    Some(format_rfc3339(st))
}

fn snapshot_age_label(scanned_at: SystemTime) -> String {
    match scanned_at.elapsed() {
        Ok(d) => format!("{} ms", d.as_millis()),
        Err(_) => "unknown".to_string(),
    }
}

// Scan orchestration

/// Sorted-by-FRN record indices (used by the size pass and orphan checks).
fn sorted_by_frn(records: &[ScanRecord]) -> Vec<u32> {
    let mut v: Vec<u32> = (0..records.len() as u32).collect();
    v.sort_unstable_by(|&a, &b| (records[a as usize].frn, a).cmp(&(records[b as usize].frn, b)));
    v
}

/// Post-processing: resolve orphans, dedupe duplicate names (8.3 short
/// names), and mark hard-link entries. Returns counts.
fn postprocess(records: &mut Vec<ScanRecord>, root_frn: u64) -> (u64, u64, u64) {
    // 1. Orphans: parent missing, or parent not a directory (or self-parent
    //    for non-root records) → attach to the root.
    let by_frn = sorted_by_frn(records);
    let mut orphans = 0u64;
    for i in 0..records.len() {
        let r = &records[i];
        let (frn, parent) = (r.frn, r.parent_frn);
        let ok_parent = if parent == frn {
            frn == root_frn
        } else {
            let j = by_frn.partition_point(|&c| records[c as usize].frn < parent);
            by_frn
                .get(j)
                .map(|&c| {
                    records[c as usize].frn == parent
                        && records[c as usize].flags & FLAG_DIRECTORY != 0
                })
                .unwrap_or(false)
        };
        if !ok_parent {
            records[i].parent_frn = root_frn;
            records[i].flags |= FLAG_ORPHANED;
            orphans += 1;
        }
    }

    // 2. Duplicate names for the same (frn, parent) — e.g. 8.3 short names —
    //    keep the longest name. Then mark every extra entry of the same FRN
    //    as a hard link.
    let n = records.len();
    let mut order: Vec<u32> = (0..n as u32).collect();
    order.sort_unstable_by(|&a, &b| {
        let ra = &records[a as usize];
        let rb = &records[b as usize];
        (ra.frn, ra.parent_frn)
            .cmp(&(rb.frn, rb.parent_frn))
            .then_with(|| {
                // Longer name first within a group.
                rb.name_len.cmp(&ra.name_len)
            })
    });
    let mut keep = vec![true; n];
    let mut duplicates = 0u64;
    let mut extra_links = 0u64;
    let mut prev_pair: Option<(u64, u64)> = None;
    let mut prev_frn: Option<u64> = None;
    let mut frn_first_seen = false;
    for &i in &order {
        let r = &records[i as usize];
        let pair = (r.frn, r.parent_frn);
        if prev_pair == Some(pair) {
            keep[i as usize] = false;
            duplicates += 1;
            continue;
        }
        prev_pair = Some(pair);
        if prev_frn == Some(r.frn) {
            if frn_first_seen {
                records[i as usize].flags |= FLAG_EXTRA_LINK;
                extra_links += 1;
            }
        } else {
            prev_frn = Some(r.frn);
            frn_first_seen = true;
        }
    }
    let mut w = 0usize;
    for i in 0..n {
        if keep[i] {
            if w != i {
                records[w] = records[i];
            }
            w += 1;
        }
    }
    records.truncate(w);
    (orphans, duplicates, extra_links)
}

/// Run the NTFS fast path: enumerate, post-process, size pass, compact,
/// index, aggregate.
fn scan_ntfs(
    plan: &ScannerPlan,
    cancel: &AtomicBool,
    progress: Option<&ScanProgress>,
) -> Result<DiskSnapshot, WinkitError> {
    let t0 = Instant::now();
    if let Some(p) = progress {
        // Denominator for the enumeration phase: total MFT records, when the
        // volume reports them. Unavailable → no percent (honest, never a guess).
        if let Some(total) = ntfs::mft_total_records(&plan.volume_root) {
            p.set_total_records(total);
        }
        p.set_phase("enumeration");
    }
    let (mut records, names, root_frn, _raw) =
        ntfs::enumerate(&plan.volume_root, cancel, progress)?;
    let enum_ms = t0.elapsed().as_millis() as u64;

    if let Some(p) = progress {
        p.set_phase("postprocess");
    }
    let (orphans, duplicates, extra_links) = postprocess(&mut records, root_frn);

    if let Some(p) = progress {
        // From here the enumerated record count is the exact denominator.
        // The records counter already sits at the enumeration total, so the
        // size pass and indexing phases report 100% of the enumeration.
        p.set_total_records(records.len() as u64);
        p.set_phase("size_pass");
    }
    let t1 = Instant::now();
    let by_frn = sorted_by_frn(&records);
    sizes::fill_sizes(
        &mut records,
        &names,
        &by_frn,
        &plan.volume_root,
        root_frn,
        cancel,
        progress,
    )?;
    let size_ms = t1.elapsed().as_millis() as u64;

    if cancel.load(Ordering::Relaxed) {
        return Err(WinkitError::cancelled("disk scan cancelled"));
    }
    if let Some(p) = progress {
        p.set_phase("indexing");
    }
    let t2 = Instant::now();
    // Drop stale records, then build the index and aggregate sizes.
    let mut w = 0usize;
    for i in 0..records.len() {
        if records[i].flags & FLAG_STALE != 0 {
            continue;
        }
        if w != i {
            records[w] = records[i];
        }
        w += 1;
    }
    let stale = records.len() - w;
    records.truncate(w);
    // New orphans may appear when a stale record was a directory.
    let by_frn2 = sorted_by_frn(&records);
    let mut new_orphans = 0u64;
    for i in 0..records.len() {
        let r = &records[i];
        let (frn, parent) = (r.frn, r.parent_frn);
        let ok_parent = if parent == frn {
            frn == root_frn
        } else {
            let j = by_frn2.partition_point(|&c| records[c as usize].frn < parent);
            by_frn2
                .get(j)
                .map(|&c| {
                    records[c as usize].frn == parent
                        && records[c as usize].flags & FLAG_DIRECTORY != 0
                })
                .unwrap_or(false)
        };
        if !ok_parent && records[i].flags & FLAG_ORPHANED == 0 {
            records[i].parent_frn = root_frn;
            records[i].flags |= FLAG_ORPHANED;
            new_orphans += 1;
        }
    }
    let tree_index = tree::TreeIndex::build(&records);
    let index_ms = t2.elapsed().as_millis() as u64;

    let mut counts = ScanCounts {
        orphans: orphans + new_orphans,
        duplicate_names: duplicates,
        hard_links: extra_links,
        stale_dropped: stale as u64,
        ..Default::default()
    };
    for r in &records {
        if r.flags & FLAG_DIRECTORY != 0 {
            counts.dirs += 1;
        } else {
            counts.files += 1;
            if r.flags & FLAG_SIZE_UNKNOWN != 0 {
                counts.size_unknown += 1;
            }
        }
        if r.flags & FLAG_REPARSE != 0 {
            counts.reparse += 1;
        }
    }
    let root_idx = tree_index
        .by_frn
        .iter()
        .position(|&c| records[c as usize].frn == root_frn)
        .map(|i| tree_index.by_frn[i] as usize)
        .unwrap_or(0);
    counts.total_logical = tree_index.aggregate[root_idx];

    let top_files_all = top_n_indices(
        &tree_index.by_frn,
        &tree_index.aggregate,
        &records,
        root_frn,
        false,
        100,
    );
    let top_folders_all = top_n_indices(
        &tree_index.by_frn,
        &tree_index.aggregate,
        &records,
        root_frn,
        true,
        100,
    );

    let counts_dirs = counts.dirs;
    let counts_files = counts.files;
    if let Some(p) = progress {
        // Final, exact progress numbers (enumeration only had "so far"
        // estimates that included hard-link duplicates and stale records).
        p.set_records(records.len() as u64);
        p.set_files(counts_files);
        p.set_dirs(counts_dirs);
    }
    let snapshot = DiskSnapshot {
        // The MFT covers the whole volume even when the caller named a
        // subdirectory, so the snapshot scope is the volume root.
        volume_root: plan.volume_root.clone(),
        scope_root: plan.volume_root.clone(),
        filesystem: plan.filesystem.clone(),
        scanner: ScannerKind::NtfsMft,
        fast_path_unavailable: None,
        records,
        names,
        root_frn,
        scanned_at: SystemTime::now(),
        timings: ScanTimings {
            enum_ms,
            size_ms,
            index_ms,
            total_ms: t0.elapsed().as_millis() as u64,
        },
        counts,
        capacity: capacity_for(&plan.volume_root),
        top_files_all,
        top_folders_all,
        tree: tree_index,
    };
    Ok(snapshot)
}

/// Precompute the top-N (size, index) pairs from the sorted index, skipping
/// the volume-root record (a directory whose aggregate is the whole volume).
fn top_n_indices(
    by_frn: &[u32],
    aggregate: &[u64],
    records: &[ScanRecord],
    root_frn: u64,
    dirs_only: bool,
    n: usize,
) -> Vec<(u64, u32)> {
    let mut top = TopK::new(n);
    for &idx in by_frn {
        let r = &records[idx as usize];
        if r.frn == root_frn {
            continue;
        }
        if dirs_only {
            if r.flags & FLAG_DIRECTORY == 0 {
                continue;
            }
        } else if r.flags & FLAG_DIRECTORY != 0 {
            continue; // files only
        }
        top.push(aggregate[idx as usize], idx);
    }
    top.into_sorted()
}

/// Scan a volume, falling back to the recursive scanner when the fast path
/// is unavailable (before any records were produced). Returns the snapshot
/// and the fast-path failure reason, if a fallback happened.
pub fn scan_volume(
    plan: &ScannerPlan,
    cancel: &AtomicBool,
    progress: Option<&ScanProgress>,
) -> Result<(DiskSnapshot, Option<String>), WinkitError> {
    scan_volume_capped(plan, cancel, progress, None)
}

/// [`scan_volume`] with an explicit fallback record cap (`None` = the
/// module default). Used by the background-scan lifecycle tests to force a
/// deterministic failure.
pub(crate) fn scan_volume_capped(
    plan: &ScannerPlan,
    cancel: &AtomicBool,
    progress: Option<&ScanProgress>,
    max_records: Option<usize>,
) -> Result<(DiskSnapshot, Option<String>), WinkitError> {
    let max_records = max_records.unwrap_or(fallback::MAX_RECORDS);
    match plan.kind {
        ScannerKind::NtfsMft => match scan_ntfs(plan, cancel, progress) {
            Ok(snap) => Ok((snap, None)),
            Err(e) if e.kind == ErrorKind::Cancelled => Err(e),
            Err(e) => {
                let reason = format!("NTFS fast path unavailable: {e}");
                // Try the fallback; if it also fails, report the original.
                // The fallback walks the caller's directory (the plan's
                // scope), never silently the whole volume, so a non-elevated
                // token degrades to a bounded classic directory scan.
                match fallback::scan_with_cap(&plan.scope_root, cancel, progress, max_records) {
                    Ok((records, names, root_frn, counts, timings)) => {
                        let tree_index = tree::TreeIndex::build(&records);
                        let top_files_all = top_n_indices(
                            &tree_index.by_frn,
                            &tree_index.aggregate,
                            &records,
                            root_frn,
                            false,
                            100,
                        );
                        let top_folders_all = top_n_indices(
                            &tree_index.by_frn,
                            &tree_index.aggregate,
                            &records,
                            root_frn,
                            true,
                            100,
                        );
                        let snapshot = DiskSnapshot {
                            volume_root: plan.volume_root.clone(),
                            scope_root: plan.scope_root.clone(),
                            filesystem: plan.filesystem.clone(),
                            scanner: ScannerKind::RecursiveFallback,
                            fast_path_unavailable: Some(reason),
                            records,
                            names,
                            root_frn,
                            scanned_at: SystemTime::now(),
                            timings,
                            counts,
                            capacity: capacity_for(&plan.volume_root),
                            top_files_all,
                            top_folders_all,
                            tree: tree_index,
                        };
                        Ok((snapshot, None))
                    }
                    Err(fb_e) => {
                        // Both paths failed: report the operative fallback
                        // failure, keeping the fast-path context.
                        Err(WinkitError::new(
                            fb_e.kind,
                            format!("{fb_e}; NTFS fast path was also unavailable: {e}"),
                        ))
                    }
                }
            }
        },
        ScannerKind::RecursiveFallback => {
            if let Some(p) = progress {
                p.set_phase("recursive_walk");
            }
            let (records, names, root_frn, counts, timings) =
                fallback::scan_with_cap(&plan.scope_root, cancel, progress, max_records)?;
            let tree_index = tree::TreeIndex::build(&records);
            let top_files_all = top_n_indices(
                &tree_index.by_frn,
                &tree_index.aggregate,
                &records,
                root_frn,
                false,
                100,
            );
            let top_folders_all = top_n_indices(
                &tree_index.by_frn,
                &tree_index.aggregate,
                &records,
                root_frn,
                true,
                100,
            );
            let snapshot = DiskSnapshot {
                volume_root: plan.volume_root.clone(),
                scope_root: plan.scope_root.clone(),
                filesystem: plan.filesystem.clone(),
                scanner: ScannerKind::RecursiveFallback,
                fast_path_unavailable: None,
                records,
                names,
                root_frn,
                scanned_at: SystemTime::now(),
                timings,
                counts,
                capacity: capacity_for(&plan.volume_root),
                top_files_all,
                top_folders_all,
                tree: tree_index,
            };
            Ok((snapshot, None))
        }
    }
}

// Cache + background scans

#[derive(Debug)]
pub struct CachedSnapshot {
    pub snapshot: Arc<DiskSnapshot>,
    pub scanned_at: SystemTime,
    pub scan_duration_ms: u64,
}

/// How many completed-scan statuses are retained per process. Bounds
/// memory: each entry holds a compact summary plus a bounded top-10
/// overview, so the history can never grow without limit.
const COMPLETED_HISTORY_MAX: usize = 32;

/// A terminal scan (done / failed / cancelled), retained so callers can
/// still poll its status after the active-scan slot is released. Bounded by
/// [`COMPLETED_HISTORY_MAX`].
#[derive(Debug)]
struct CompletedScan {
    scan_id: String,
    volume_root: String,
    phase: String,
    records_so_far: u64,
    files_so_far: u64,
    directories_so_far: u64,
    elapsed_ms: u64,
    error: Option<String>,
    result: Option<DiskScanInfo>,
}

impl CompletedScan {
    fn to_status(&self) -> DiskScanStatusInfo {
        // A completed scan either reached 100% (phase "done") or never
        // finished (cancelled/failed) — only "done" claims completion.
        let (progress_percent, eta_seconds) = if self.phase == "done" {
            (Some(100.0), Some(0))
        } else {
            (None, None)
        };
        DiskScanStatusInfo {
            scan_id: self.scan_id.clone(),
            volume: self.volume_root.clone(),
            phase: self.phase.clone(),
            records_so_far: self.records_so_far,
            files_so_far: self.files_so_far,
            directories_so_far: self.directories_so_far,
            elapsed_ms: self.elapsed_ms,
            progress_percent,
            eta_seconds,
            done: true,
            cancelled: self.phase == "cancelled",
            error: self.error.clone(),
            result: self.result.clone(),
        }
    }
}

#[derive(Debug)]
struct RunningScan {
    scan_id: String,
    volume_root: String,
    /// Cache key: the scope that was actually scanned (a subdirectory for
    /// a scoped recursive fallback, the volume root otherwise).
    cache_key: String,
    cancel: Arc<AtomicBool>,
    progress: Arc<ScanProgress>,
    started_at: Instant,
    /// Published before the worker releases the active slot, so a poll in
    /// the window between "failed" and removal still sees the error.
    error: Mutex<Option<String>>,
}

/// Per-volume snapshot cache plus background-scan tracking. Owned by the
/// real Windows backend so every tool call reuses one snapshot.
///
/// Lifecycle contract:
/// * the `running` map holds only *active* scans — completed, failed, and
///   cancelled scans are removed by their worker thread, so a new scan for
///   the same scope can start as soon as the previous one ends;
/// * terminal outcomes are retained in a bounded `completed` history so
///   `disk_scan_status` can still answer for a finished scan_id;
/// * `errors` never accumulates: failure messages live on the running entry
///   and in the bounded completed history only.
#[derive(Debug, Default)]
pub struct DiskScanService {
    cache: Mutex<HashMap<String, CachedSnapshot>>,
    running: Mutex<HashMap<String, RunningScan>>,
    completed: Mutex<std::collections::VecDeque<CompletedScan>>,
    next_id: AtomicU64,
    /// Fallback record cap override (`None` = production default). A test
    /// seam that lets the lifecycle tests force a deterministic scan
    /// failure without touching production behavior.
    pub(crate) fallback_cap: Option<usize>,
}

impl DiskScanService {
    /// Force the recursive-fallback record cap (test seam).
    #[cfg(test)]
    pub(crate) fn set_fallback_cap(&mut self, cap: usize) {
        self.fallback_cap = Some(cap);
    }

    pub(crate) fn capped_scan(
        &self,
        plan: &ScannerPlan,
        cancel: &AtomicBool,
        progress: Option<&ScanProgress>,
    ) -> Result<(DiskSnapshot, Option<String>), WinkitError> {
        scan_volume_capped(plan, cancel, progress, self.fallback_cap)
    }
}

impl DiskScanService {
    /// Synchronous scan-or-serve-from-cache. `refresh: true` forces a
    /// rescan; otherwise a snapshot younger than `max_age_ms` is reused.
    pub fn sync_scan(&self, request: &DiskScanRequest) -> Result<DiskScanInfo, WinkitError> {
        let plan = plan_for_path(&request.path)?;
        let key = plan.scope_root.clone();
        let max_age = if request.max_age_ms == 0 {
            DEFAULT_CACHE_TTL_MS
        } else {
            request.max_age_ms
        };
        if !request.refresh {
            if let Some(c) = self.cache.lock().unwrap().get(&key) {
                if let Ok(age) = c.scanned_at.elapsed() {
                    if age.as_millis() as u64 <= max_age {
                        return Ok(c.snapshot.info(true, Some(age.as_millis() as u64)));
                    }
                }
            }
        }
        let cancel = Arc::new(AtomicBool::new(false));
        let progress = Arc::new(ScanProgress::new());
        let (snapshot, _) = self.capped_scan(&plan, &cancel, Some(&progress))?;
        let duration = snapshot.timings.total_ms;
        let snap = Arc::new(snapshot);
        let cached = CachedSnapshot {
            snapshot: snap.clone(),
            scanned_at: SystemTime::now(),
            scan_duration_ms: duration,
        };
        self.cache.lock().unwrap().insert(key, cached);
        Ok(snap.info(false, None))
    }

    /// Start a background scan. Returns the initial status. Repeated calls
    /// for the same scope while a scan is *running* return that scan's
    /// status; once the previous scan is terminal, a new scan starts.
    pub fn start(
        self: &Arc<Self>,
        request: &DiskScanRequest,
    ) -> Result<DiskScanStatusInfo, WinkitError> {
        let plan = plan_for_path(&request.path)?;
        let key = plan.scope_root.clone();
        {
            let mut running = self.running.lock().unwrap();
            if let Some(rs) = running.get(&key) {
                let phase = rs.progress.phase();
                if matches!(phase.as_str(), "done" | "failed" | "cancelled") {
                    // Terminal entry still in the map (worker between
                    // finishing and removal): treat as not running so a
                    // new scan is never blocked by a finished one.
                    running.remove(&key);
                } else {
                    return Ok(self.status_for(rs));
                }
            }
        }
        let scan_id = format!("scan-{}", self.next_id.fetch_add(1, Ordering::Relaxed));
        let cancel = Arc::new(AtomicBool::new(false));
        let progress = Arc::new(ScanProgress::new());
        progress.set_phase("starting");
        let running = RunningScan {
            scan_id: scan_id.clone(),
            volume_root: plan.volume_root.clone(),
            cache_key: key.clone(),
            cancel: cancel.clone(),
            progress: progress.clone(),
            started_at: Instant::now(),
            error: Mutex::new(None),
        };
        self.running.lock().unwrap().insert(key.clone(), running);

        let this = self.clone();
        let closure_key = key.clone();
        let closure_scan_id = scan_id.clone();
        let closure_volume = plan.volume_root.clone();
        let fallback_cap = self.fallback_cap;
        tokio::task::spawn_blocking(move || {
            let scan_started = Instant::now();
            let outcome = scan_volume_capped(&plan, &cancel, Some(&progress), fallback_cap);
            let (phase, error) = match outcome {
                Ok((snapshot, _)) => {
                    progress.set_phase("done");
                    let duration = snapshot.timings.total_ms;
                    let snap = Arc::new(snapshot);
                    let cached = CachedSnapshot {
                        snapshot: snap.clone(),
                        scanned_at: SystemTime::now(),
                        scan_duration_ms: duration,
                    };
                    this.cache
                        .lock()
                        .unwrap()
                        .insert(closure_key.clone(), cached);
                    ("done", None)
                }
                Err(e) => {
                    let is_cancel = e.kind == ErrorKind::Cancelled;
                    let phase = if is_cancel { "cancelled" } else { "failed" };
                    progress.set_phase(phase);
                    // A cancellation is not an error: only failed scans
                    // carry an error string (on the active entry and in the
                    // completed history).
                    if !is_cancel {
                        let err = e.to_string();
                        if let Some(rs) = this.running.lock().unwrap().get(&closure_key) {
                            if rs.scan_id == closure_scan_id {
                                *rs.error.lock().unwrap() = Some(err.clone());
                            }
                        }
                        (phase, Some(err))
                    } else {
                        (phase, None)
                    }
                }
            };
            // The completed status's embedded overview is the scan's own
            // snapshot (not whatever a later rescan replaced in the cache).
            let result = this
                .cache
                .lock()
                .unwrap()
                .get(&closure_key)
                .map(|c| c.snapshot.info(true, Some(0)));
            // Retain the terminal outcome in the bounded history FIRST, so a
            // status poll can never observe the scan in neither map (the
            // active slot is released only after the outcome is queryable).
            {
                let mut completed = this.completed.lock().unwrap();
                completed.push_back(CompletedScan {
                    scan_id: closure_scan_id.clone(),
                    volume_root: closure_volume,
                    phase: phase.to_string(),
                    records_so_far: progress.records(),
                    files_so_far: progress.files(),
                    directories_so_far: progress.dirs(),
                    elapsed_ms: scan_started.elapsed().as_millis() as u64,
                    error,
                    result,
                });
                while completed.len() > COMPLETED_HISTORY_MAX {
                    completed.pop_front();
                }
            }
            // Release the active-scan slot: a finished scan must never stay
            // in `running` (it would block a new scan for the same scope).
            // Only remove the entry we own — a newer scan for the same scope
            // must never be evicted by an older worker.
            {
                let mut running = this.running.lock().unwrap();
                if let Some(rs) = running.get(&closure_key) {
                    if rs.scan_id == closure_scan_id {
                        running.remove(&closure_key);
                    }
                }
            }
        });
        // Read the initial status back. The worker can finish and release
        // the slot before we get here (tiny trees finish in microseconds),
        // so fall back to the completed entry — never a panicking re-lock:
        // the running lock must be dropped before `status()` (Mutex is not
        // reentrant).
        let from_running = {
            let running = self.running.lock().unwrap();
            match running.get(&key) {
                Some(rs) if rs.scan_id == scan_id => Some(self.status_for(rs)),
                _ => None,
            }
        };
        let status = match from_running {
            Some(s) => s,
            None => self
                .status(&scan_id)
                .expect("scan finished before initial status read"),
        };
        Ok(status)
    }

    fn status_for(&self, rs: &RunningScan) -> DiskScanStatusInfo {
        let phase = rs.progress.phase();
        let elapsed = rs.started_at.elapsed().as_millis() as u64;
        let done = matches!(phase.as_str(), "done" | "failed" | "cancelled");
        let result = if phase == "done" {
            self.cache.lock().unwrap().get(&rs.cache_key).map(|c| {
                c.snapshot.info(
                    true,
                    c.scanned_at.elapsed().ok().map(|d| d.as_millis() as u64),
                )
            })
        } else {
            None
        };
        let error = if phase == "failed" {
            rs.error.lock().unwrap().clone()
        } else {
            None
        };
        // Only a scan that actually reached "done" claims 100%; cancelled and
        // failed scans never report a completion percent.
        let (progress_percent, eta_seconds) = if phase == "done" {
            (Some(100.0), Some(0))
        } else {
            (
                rs.progress.progress_percent(),
                rs.progress.eta_seconds(elapsed),
            )
        };
        DiskScanStatusInfo {
            scan_id: rs.scan_id.clone(),
            volume: rs.volume_root.clone(),
            phase: phase.clone(),
            records_so_far: rs.progress.records(),
            files_so_far: rs.progress.files(),
            directories_so_far: rs.progress.dirs(),
            elapsed_ms: elapsed,
            progress_percent,
            eta_seconds,
            done,
            cancelled: phase == "cancelled",
            error,
            result,
        }
    }

    pub fn status(&self, scan_id: &str) -> Option<DiskScanStatusInfo> {
        {
            let running = self.running.lock().unwrap();
            if let Some(rs) = running.values().find(|rs| rs.scan_id == scan_id) {
                return Some(self.status_for(rs));
            }
        }
        let completed = self.completed.lock().unwrap();
        completed
            .iter()
            .rev()
            .find(|c| c.scan_id == scan_id)
            .map(CompletedScan::to_status)
    }

    pub fn cancel(&self, scan_id: &str) -> bool {
        let running = self.running.lock().unwrap();
        if let Some(rs) = running.values().find(|rs| rs.scan_id == scan_id) {
            rs.cancel.store(true, Ordering::Relaxed);
            true
        } else {
            false
        }
    }

    /// The cached snapshot for the volume containing `path`, scanning
    /// synchronously if none exists yet. Used by query tools.
    pub fn ensure_snapshot(&self, path: &str) -> Result<Arc<DiskSnapshot>, WinkitError> {
        let plan = plan_for_path(path)?;
        let key = plan.scope_root.clone();
        if let Some(c) = self.cache.lock().unwrap().get(&key) {
            return Ok(c.snapshot.clone());
        }
        let cancel = Arc::new(AtomicBool::new(false));
        let (snapshot, _) = self.capped_scan(&plan, &cancel, None)?;
        let duration = snapshot.timings.total_ms;
        let snap = Arc::new(snapshot);
        let cached = CachedSnapshot {
            snapshot: snap.clone(),
            scanned_at: SystemTime::now(),
            scan_duration_ms: duration,
        };
        self.cache.lock().unwrap().insert(key, cached);
        Ok(snap)
    }

    /// Run one query against the snapshot of the volume containing `path`.
    pub fn query(
        &self,
        request: &crate::models::DiskQueryRequest,
    ) -> Result<DiskQueryResult, WinkitError> {
        let snap = self.ensure_snapshot(&request.path)?;
        let age = snap.scanned_at.elapsed().ok().map(|d| d.as_millis() as u64);
        let scanner = snap.scanner.as_str().to_string();
        let fast_path_unavailable = snap.fast_path_unavailable.clone();
        let volume = snap.volume_root.clone();
        let root_idx = snap.root_index();
        let idx = snap.resolve_path(&request.path)?;
        // Subtree queries only when the requested path is not the volume root.
        let subtree = if Some(idx) == root_idx {
            None
        } else {
            Some(idx)
        };
        let exts = request.extensions.as_deref();
        let result = match request.kind {
            crate::models::DiskQueryKind::TopFiles => {
                let entries = snap.top_files(subtree, request.limit, request.min_size_bytes, exts);
                DiskQueryResult::TopFiles {
                    entries,
                    volume,
                    scanner,
                    fast_path_unavailable: fast_path_unavailable.clone(),
                    cached: true,
                    snapshot_age_ms: age,
                }
            }
            crate::models::DiskQueryKind::TopFolders => {
                let entries = snap.top_folders(subtree, request.limit);
                DiskQueryResult::TopFolders {
                    entries,
                    volume,
                    scanner,
                    fast_path_unavailable: fast_path_unavailable.clone(),
                    cached: true,
                    snapshot_age_ms: age,
                }
            }
            crate::models::DiskQueryKind::FolderSize => {
                let folder = snap.folder_size(idx);
                DiskQueryResult::FolderSize {
                    folder,
                    volume,
                    scanner,
                    fast_path_unavailable: fast_path_unavailable.clone(),
                    cached: true,
                    snapshot_age_ms: age,
                }
            }
            crate::models::DiskQueryKind::FindFiles => {
                let (entries, truncated) = snap.find_files(
                    subtree,
                    request.pattern.as_deref(),
                    exts,
                    request.min_size_bytes,
                    request.limit,
                );
                DiskQueryResult::FindFiles {
                    entries,
                    truncated,
                    volume,
                    scanner,
                    fast_path_unavailable: fast_path_unavailable.clone(),
                    cached: true,
                    snapshot_age_ms: age,
                }
            }
        };
        Ok(result)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::DiskQueryKind;

    fn rec(
        frn: u64,
        parent: u64,
        size: u64,
        is_dir: bool,
        reparse: bool,
        name: &str,
        names: &mut Vec<u8>,
    ) -> ScanRecord {
        let off = names.len() as u32;
        names.extend_from_slice(name.as_bytes());
        let mut flags = 0u8;
        if is_dir {
            flags |= FLAG_DIRECTORY;
        }
        if reparse {
            flags |= FLAG_REPARSE;
        }
        ScanRecord {
            frn,
            parent_frn: parent,
            size,
            mtime: 1_700_000_000,
            name_off: off,
            name_len: name.len() as u16,
            attributes: 0,
            flags,
        }
    }

    /// Synthetic volume: root (5), dirs Games(10)/Docs(20)/Junction(30,
    /// reparse), files incl. a 50 GB iso with a hard link under Docs, an
    /// orphan whose parent is missing, and a reparse file.
    fn build_snapshot() -> DiskSnapshot {
        let mut names = Vec::new();
        let mut records = vec![
            rec(5, 5, 0, true, false, "", &mut names),
            rec(10, 5, 0, true, false, "Games", &mut names),
            rec(20, 5, 0, true, false, "Docs", &mut names),
            rec(30, 5, 0, true, true, "Junction", &mut names),
            rec(40, 10, 5_000, false, false, "small.iso", &mut names),
            rec(50, 10, 50_000_000_000, false, false, "big.iso", &mut names),
            rec(60, 20, 2_000, false, false, "readme.md", &mut names),
            rec(70, 30, 1_000, false, true, "link.txt", &mut names),
            rec(50, 20, 50_000_000_000, false, false, "big2.iso", &mut names),
            rec(80, 999, 700, false, false, "orphan.bin", &mut names),
            rec(90, 20, 300, false, false, "archive.zip", &mut names),
        ];
        postprocess(&mut records, 5);
        let tree = tree::TreeIndex::build(&records);
        let top_files_all = top_n_indices(&tree.by_frn, &tree.aggregate, &records, 5, false, 100);
        let top_folders_all = top_n_indices(&tree.by_frn, &tree.aggregate, &records, 5, true, 100);
        let mut counts = ScanCounts::default();
        for r in &records {
            if r.flags & FLAG_DIRECTORY != 0 {
                counts.dirs += 1;
            } else {
                counts.files += 1;
            }
        }
        let root_idx = tree
            .by_frn
            .iter()
            .position(|&c| records[c as usize].frn == 5)
            .map(|i| tree.by_frn[i] as usize)
            .unwrap_or(0);
        counts.total_logical = tree.aggregate[root_idx];
        DiskSnapshot {
            volume_root: "D:\\".into(),
            scope_root: "D:\\".into(),
            filesystem: "NTFS".into(),
            scanner: ScannerKind::NtfsMft,
            fast_path_unavailable: None,
            records,
            names,
            root_frn: 5,
            scanned_at: SystemTime::now(),
            timings: ScanTimings::default(),
            counts,
            capacity: None,
            top_files_all,
            top_folders_all,
            tree,
        }
    }

    #[test]
    fn postprocess_attaches_orphans_and_marks_hard_links() {
        let mut names = Vec::new();
        let mut records = vec![
            rec(5, 5, 0, true, false, "", &mut names),
            // Parent directories for the hard links below.
            rec(10, 5, 0, true, false, "A", &mut names),
            rec(20, 5, 0, true, false, "B", &mut names),
            // Hard links: same FRN, different parents.
            rec(50, 10, 0, false, false, "a.bin", &mut names),
            rec(50, 20, 0, false, false, "b.bin", &mut names),
            // Duplicate (long/short) names: same FRN, same parent.
            rec(60, 5, 0, false, false, "LongName.txt", &mut names),
            rec(60, 5, 0, false, false, "LONGNA~1.TXT", &mut names),
            // Orphan: parent 999 missing.
            rec(70, 999, 0, false, false, "orphan.dat", &mut names),
            // Self-parent (non-root): treated as orphan.
            rec(80, 80, 0, false, false, "weird.dat", &mut names),
        ];
        let (orphans, duplicates, extra_links) = postprocess(&mut records, 5);
        assert_eq!(orphans, 2);
        assert_eq!(duplicates, 1);
        assert_eq!(extra_links, 1);
        let orphan = records.iter().find(|r| r.frn == 70).unwrap();
        assert_eq!(orphan.parent_frn, 5);
        assert_ne!(orphan.flags & FLAG_ORPHANED, 0);
        let weird = records.iter().find(|r| r.frn == 80).unwrap();
        assert_eq!(weird.parent_frn, 5);
        let dup_kept = records.iter().find(|r| r.frn == 60).unwrap();
        assert_eq!(dup_kept.name_len, "LongName.txt".len() as u16);
        let hard = records
            .iter()
            .find(|r| r.frn == 50 && r.parent_frn == 20)
            .unwrap();
        assert_ne!(hard.flags & FLAG_EXTRA_LINK, 0);
        let first = records
            .iter()
            .find(|r| r.frn == 50 && r.parent_frn == 10)
            .unwrap();
        assert_eq!(first.flags & FLAG_EXTRA_LINK, 0);
        // 8 kept: root, A, B, 2×frn50, 1×frn60, orphan, weird = 8
        assert_eq!(records.len(), 8);
    }

    #[test]
    fn queries_aggregate_and_order_correctly() {
        let snap = build_snapshot();
        // Games = small.iso + big.iso = 50_000_005_000; Docs = readme + big2 + archive = 50_000_002_300.
        let games = snap.resolve_path("D:\\Games").unwrap();
        assert_eq!(snap.folder_size(games).size_bytes, 50_000_005_000);
        assert_eq!(snap.folder_size(games).files, 2);
        let docs = snap.resolve_path("D:\\Docs").unwrap();
        assert_eq!(snap.folder_size(docs).size_bytes, 50_000_002_300);

        // Largest folders on the volume: Games then Docs (Junction has 0).
        let folders = snap.top_folders(None, 3);
        assert_eq!(folders[0].path, "D:\\Games");
        assert_eq!(folders[0].size_bytes, 50_000_005_000);
        assert_eq!(folders[1].path, "D:\\Docs");

        // Largest files on the volume: the two 50 GB hard links first.
        let files = snap.top_files(None, 3, 0, None);
        assert_eq!(files[0].size_bytes, 50_000_000_000);
        assert_eq!(files[0].hard_links, 2);
        assert_eq!(files[0].path, "D:\\Games\\big.iso");
        assert_eq!(files[1].path, "D:\\Docs\\big2.iso");
        assert_eq!(files[2].path, "D:\\Games\\small.iso"); // 5 KB beats 300 B

        // Subtree query: largest under Games only.
        let sub = snap.top_files(Some(games), 10, 0, None);
        assert_eq!(sub.len(), 2);
        assert!(sub.iter().all(|f| f.path.starts_with("D:\\Games\\")));

        // Whole-volume total equals the root aggregate.
        let root = snap.root_index().unwrap();
        assert_eq!(
            snap.tree.aggregate[root as usize],
            snap.counts.total_logical
        );
        // Everything sums: 5000 + 50e9 + 2000 + 1000 + 50e9 + 700 + 300.
        assert_eq!(snap.counts.total_logical, 100_000_009_000);
    }

    #[test]
    fn folder_size_and_resolve_path_handle_root() {
        let snap = build_snapshot();
        let root = snap.resolve_path("D:").unwrap();
        assert_eq!(root, snap.root_index().unwrap());
        let root2 = snap.resolve_path("D:\\").unwrap();
        assert_eq!(root2, root);
        // Case-insensitive resolution.
        let games = snap.resolve_path("d:\\games").unwrap();
        assert_eq!(snap.path_of(games), "D:\\Games");
        // Missing path.
        let err = snap.resolve_path("D:\\Nope").unwrap_err();
        assert_eq!(err.kind, crate::errors::ErrorKind::NotFound);
    }

    #[test]
    fn find_files_filters_and_truncates() {
        let snap = build_snapshot();
        let (found, truncated) = snap.find_files(None, Some("*.iso"), None, 0, 10);
        assert_eq!(found.len(), 3);
        assert!(!truncated);
        assert!(found.iter().all(|f| f.path.ends_with(".iso")));
        // min-size filter keeps only the 50 GB hard links.
        let (big, _) = snap.find_files(None, None, None, 1_000_000, 10);
        assert_eq!(big.len(), 2); // big.iso, big2.iso
                                  // truncation: limit 2 with 3 iso matches.
        let (few, truncated) = snap.find_files(None, Some("*.iso"), None, 0, 2);
        assert_eq!(few.len(), 2);
        assert!(truncated);
        // extensions filter.
        let (zips, _) = snap.find_files(None, None, Some(&["ZIP".to_string()]), 0, 10);
        assert_eq!(zips.len(), 1);
        assert_eq!(zips[0].path, "D:\\Docs\\archive.zip");
        // subtree find.
        let games = snap.resolve_path("D:\\Games").unwrap();
        let (sub, _) = snap.find_files(Some(games), None, None, 0, 10);
        assert_eq!(sub.len(), 2);
    }

    #[test]
    fn reparse_entries_are_flagged() {
        let snap = build_snapshot();
        let junc = snap.resolve_path("D:\\Junction").unwrap();
        // In this synthetic model Junction has one child; in real NTFS a
        // junction has no MFT children (its descendants live under the
        // target), which the enumerator reflects by construction.
        assert_eq!(snap.folder_size(junc).size_bytes, 1_000);
        let entry = snap.top_files(None, 100, 0, None);
        let reparse = entry
            .iter()
            .find(|e| e.path == "D:\\Junction\\link.txt")
            .unwrap();
        assert!(reparse.is_reparse_point);
        let _ = snap.resolve_path("D:\\Junction\\link.txt").unwrap();
    }

    #[test]
    fn normalize_and_volume_root_helpers() {
        assert_eq!(normalize_path("D:").unwrap(), "D:\\");
        assert_eq!(normalize_path("d:\\Games").unwrap(), "D:\\Games");
        assert_eq!(
            normalize_path("D:\\Games\\Steam").unwrap(),
            "D:\\Games\\Steam"
        );
        assert_eq!(volume_root_of("D:\\Games"), "D:\\");
        assert_eq!(volume_root_of(r"\\srv\share\x"), r"\\srv\share");
        assert!(normalize_path("").is_err());
    }

    #[test]
    fn query_service_rejects_missing_snapshot_paths() {
        // The query path itself must exist in the snapshot.
        let snap = build_snapshot();
        let err = snap.resolve_path("E:\\whatever").unwrap_err();
        assert_eq!(err.kind, crate::errors::ErrorKind::NotFound);
        let _ = DiskQueryKind::TopFiles; // keep import used
    }

    // Background-scan lifecycle + progress (deterministic, real temp dirs)

    /// A unique temp root for one scan test, with `dirs` subdirectories each
    /// holding `files_per_dir` empty files. Returns the normalized path.
    fn temp_scan_tree(tag: &str, dirs: usize, files_per_dir: usize) -> String {
        let stamp = SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "winkit_disk_scan_{tag}_{}_{}",
            std::process::id(),
            stamp
        ));
        std::fs::create_dir_all(&root).unwrap();
        for d in 0..dirs {
            let sub = root.join(format!("dir{d:03}"));
            std::fs::create_dir_all(&sub).unwrap();
            for f in 0..files_per_dir {
                let path = sub.join(format!("file{f:04}.bin"));
                std::fs::File::create(&path).unwrap();
            }
        }
        // One file directly in the root as well, so files > dirs.
        std::fs::write(root.join("root.bin"), vec![0u8; 7]).unwrap();
        root.to_string_lossy().into_owned()
    }

    fn remove_tree(root: &str) {
        std::fs::remove_dir_all(root).ok();
    }

    fn scan_request(root: &str) -> DiskScanRequest {
        DiskScanRequest {
            path: root.to_string(),
            refresh: true,
            max_age_ms: 0,
        }
    }

    /// Poll `disk_scan_status` until the scan is terminal, up to 30 s.
    fn wait_terminal(service: &DiskScanService, scan_id: &str) -> DiskScanStatusInfo {
        for _ in 0..3000 {
            if let Some(s) = service.status(scan_id) {
                if s.done {
                    return s;
                }
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        panic!("scan {scan_id} did not reach a terminal state within 30 s");
    }

    /// Poll until the active-scan map drains (the worker releases the slot
    /// just after publishing the completed entry), up to 5 s.
    fn wait_running_empty(service: &DiskScanService) {
        for _ in 0..500 {
            if service.running.lock().unwrap().is_empty() {
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        panic!(
            "active scan map did not drain: {} entries remain",
            service.running.lock().unwrap().len()
        );
    }

    #[test]
    fn scan_progress_reports_phase_records_files_and_dirs() {
        let p = ScanProgress::new();
        assert_eq!(p.phase(), "starting");
        p.set_phase("enumeration");
        assert_eq!(p.phase(), "enumeration");
        p.set_records(1000);
        p.set_files(900);
        p.set_dirs(100);
        assert_eq!(p.records(), 1000);
        assert_eq!(p.files(), 900);
        assert_eq!(p.dirs(), 100);
    }

    #[test]
    fn progress_percent_and_eta_are_honest_and_deterministic() {
        let p = ScanProgress::new();
        // No total: percent is None, never a guess.
        p.set_records(1000);
        assert_eq!(p.progress_percent(), None);
        assert_eq!(p.eta_seconds(10_000), None);

        p.set_total_records(200);
        p.set_records(50);
        assert_eq!(p.progress_percent(), Some(25.0));
        p.set_records(100);
        assert_eq!(p.progress_percent(), Some(50.0));

        // pct = 25, elapsed 10_000 ms → (75/25) * 10_000 / 1000 = 30 s.
        p.set_records(50);
        assert_eq!(p.eta_seconds(10_000), Some(30));

        // Numerator above the total clamps to 100.
        p.set_records(250);
        assert_eq!(p.progress_percent(), Some(100.0));
        assert_eq!(p.eta_seconds(10_000), Some(0));

        // Zero progress gives no ETA.
        p.set_records(0);
        assert_eq!(p.progress_percent(), Some(0.0));
        assert_eq!(p.eta_seconds(10_000), None);
    }

    #[test]
    fn fallback_walk_reports_nonzero_directory_progress() {
        let root = temp_scan_tree("prog", 2, 3);
        let cancel = AtomicBool::new(false);
        let progress = ScanProgress::new();
        let (_, _, _, counts, _) = fallback::scan(&root, &cancel, Some(&progress)).unwrap();
        assert!(counts.dirs > 0, "expected directories in the tree");
        assert!(counts.files > 0, "expected files in the tree");
        // The walk published exact totals to the shared progress handle.
        assert_eq!(progress.dirs(), counts.dirs);
        assert_eq!(progress.files(), counts.files);
        assert_eq!(progress.records(), counts.files + counts.dirs);
        remove_tree(&root);
    }

    #[tokio::test]
    async fn background_scan_completes_status_remains_readable_and_reports_dirs() {
        let root = temp_scan_tree("complete", 3, 4);
        let service = Arc::new(DiskScanService::default());
        let st = service.start(&scan_request(&root)).unwrap();
        let scan_id = st.scan_id.clone();
        let done = wait_terminal(&service, &scan_id);
        assert_eq!(done.phase, "done");
        assert!(done.done);
        assert!(!done.cancelled);
        assert!(done.error.is_none());
        // Directory progress is reported (was hardcoded to 0 before).
        assert!(
            done.directories_so_far > 0,
            "directories_so_far must be nonzero: {done:?}"
        );
        assert!(done.files_so_far > 0, "files_so_far: {done:?}");
        assert!(done.records_so_far >= done.files_so_far + done.directories_so_far);
        // The completed scan's embedded summary is present.
        let result = done.result.expect("completed scan has a result");
        assert_eq!(result.scanner, "recursive_fallback");
        assert!(result.files_indexed > 0);
        assert!(result.directories_indexed > 0);
        // Status stays readable after the active slot was released.
        let again = service.status(&scan_id).expect("status remains readable");
        assert_eq!(again.phase, "done");
        assert!(again.result.is_some());
        // The running map no longer holds the finished scan. The worker
        // releases the active slot right after publishing the completed
        // entry, so this is an async property: poll until it drains.
        wait_running_empty(&service);
        remove_tree(&root);
    }

    #[tokio::test]
    async fn second_scan_starts_after_first_completes() {
        let root = temp_scan_tree("twice", 2, 2);
        let service = Arc::new(DiskScanService::default());
        let first = service.start(&scan_request(&root)).unwrap();
        let done1 = wait_terminal(&service, &first.scan_id);
        assert_eq!(done1.phase, "done");

        // A new scan for the same scope starts and runs to completion.
        let second = service.start(&scan_request(&root)).unwrap();
        assert_ne!(second.scan_id, first.scan_id, "new scan gets a new id");
        let done2 = wait_terminal(&service, &second.scan_id);
        assert_eq!(done2.phase, "done");
        // Both statuses remain readable from the bounded history.
        assert!(service.status(&first.scan_id).is_some());
        assert!(service.status(&second.scan_id).is_some());
        remove_tree(&root);
    }

    #[tokio::test]
    async fn failed_scan_does_not_block_future_scans() {
        let root = temp_scan_tree("fail", 1, 8);
        let mut svc = DiskScanService::default();
        // Force the recursive walk to fail at the 3rd record: a deterministic
        // async failure, the same lifecycle path as any scan error.
        svc.set_fallback_cap(2);
        let service = Arc::new(svc);
        let st = service.start(&scan_request(&root)).unwrap();
        let failed = wait_terminal(&service, &st.scan_id);
        assert_eq!(failed.phase, "failed");
        assert!(failed.done);
        let err = failed.error.as_deref().unwrap_or("");
        assert!(
            err.contains("exceeded"),
            "failure must be the forced record cap: {err}"
        );
        // The failed scan released the active slot: a new scan starts (and
        // runs to its own terminal state instead of echoing the old one).
        let st2 = service.start(&scan_request(&root)).unwrap();
        assert_ne!(st2.scan_id, st.scan_id);
        let f2 = wait_terminal(&service, &st2.scan_id);
        assert_eq!(f2.phase, "failed"); // still capped: fails again, but ran
                                        // The original failure stays readable in history.
        let again = service.status(&st.scan_id).expect("failed scan readable");
        assert_eq!(again.phase, "failed");
        assert!(again.error.is_some());
        remove_tree(&root);
    }

    #[tokio::test]
    async fn cancelled_scan_releases_slot_for_future_scans() {
        // Large enough that a mid-walk cancel is reliably observed.
        let root = temp_scan_tree("cancel", 20, 400);
        let service = Arc::new(DiskScanService::default());
        let st = service.start(&scan_request(&root)).unwrap();
        let scan_id = st.scan_id.clone();
        // Wait until the walk is demonstrably mid-flight (records > 0).
        let mut observed = 0u64;
        for _ in 0..2000 {
            if let Some(s) = service.status(&scan_id) {
                if s.records_so_far > 0 {
                    observed = s.records_so_far;
                    break;
                }
            }
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        assert!(observed > 0, "scan never started walking");
        assert!(service.cancel(&scan_id), "cancel of a running scan");
        let cancelled = wait_terminal(&service, &scan_id);
        assert_eq!(
            cancelled.phase, "cancelled",
            "mid-walk cancel must be observed: {cancelled:?}"
        );
        assert!(cancelled.cancelled);
        assert!(cancelled.error.is_none());
        // The cancelled scan released the slot: a new scan runs to completion.
        let st2 = service.start(&scan_request(&root)).unwrap();
        assert_ne!(st2.scan_id, scan_id);
        let done = wait_terminal(&service, &st2.scan_id);
        assert_eq!(done.phase, "done");
        remove_tree(&root);
    }

    #[tokio::test]
    async fn repeated_scans_do_not_leak_state() {
        let root = temp_scan_tree("leak", 1, 1);
        let service = Arc::new(DiskScanService::default());
        let mut ids = Vec::new();
        // More scans than the history bound: the queue must stay capped.
        for _ in 0..COMPLETED_HISTORY_MAX + 4 {
            let st = service.start(&scan_request(&root)).unwrap();
            let done = wait_terminal(&service, &st.scan_id);
            assert_eq!(done.phase, "done");
            ids.push(st.scan_id.clone());
        }
        // Active-scan state is fully released after every run (the worker
        // removes the slot right after publishing the completed entry).
        wait_running_empty(&service);
        // History is bounded: the newest scans are readable, the oldest were
        // trimmed away.
        let completed = service.completed.lock().unwrap();
        assert_eq!(completed.len(), COMPLETED_HISTORY_MAX);
        drop(completed);
        assert!(service.status(&ids[0]).is_none(), "oldest was trimmed");
        let last = ids.last().unwrap();
        assert!(
            service.status(last).is_some(),
            "newest scan remains readable"
        );
        remove_tree(&root);
    }

    #[tokio::test]
    async fn scoped_fallback_snapshot_covers_only_the_requested_directory() {
        let root = temp_scan_tree("scope", 2, 3);
        let service = Arc::new(DiskScanService::default());
        let snap = service.ensure_snapshot(&root).unwrap();
        assert_eq!(snap.scope_root, root, "fallback is scoped to the path");
        assert_eq!(snap.scanner, ScannerKind::RecursiveFallback);
        assert!(
            snap.fast_path_unavailable.is_some(),
            "scoped fallback reports why the fast path was not used"
        );
        // Queries resolve against the scoped snapshot.
        let idx = snap.resolve_path(&root).unwrap();
        assert_eq!(idx, snap.root_index().unwrap());
        let files = snap.top_files(None, 10, 0, None);
        assert_eq!(files.len(), 7, "2 dirs x 3 files + root.bin");
        assert!(files.iter().all(|f| f.path.starts_with(&root)));
        remove_tree(&root);
    }
}

/// Live NTFS integration tests and the benchmark harness (opt-in):
/// `WINKIT_LIVE_WINDOWS=1 cargo test --features live-windows`. Runs against a
/// real NTFS volume (the first fixed NTFS drive found, preferring C:).
#[cfg(all(test, feature = "live-windows"))]
mod live_windows {
    use super::*;
    use windows_sys::Win32::System::ProcessStatus::{
        K32GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS,
    };
    use windows_sys::Win32::System::Threading::GetCurrentProcess;

    fn live_enabled() -> bool {
        std::env::var("WINKIT_LIVE_WINDOWS")
            .map(|v| v == "1")
            .unwrap_or(false)
    }

    /// Structured live-test result line. The output must explicitly say
    /// which of PASS / SKIP / FAIL / BLOCKED happened — a skipped fast-path
    /// test is never counted as proof of performance.
    fn live_report(kind: &str, msg: &str) {
        eprintln!("[live] {kind}: {msg}");
    }

    fn live_skip(reason: &str) {
        live_report("SKIP", reason);
    }

    /// Probe whether the NTFS fast path is usable: attempt the `GENERIC_READ`
    /// volume open that `FSCTL_ENUM_USN_DATA` requires. `Ok(())` means a real
    /// MFT scan can run; `Err` carries the exact reason (usually Win32 error 5,
    /// access denied, on an unprivileged token).
    fn fast_path_probe(vol: &str) -> Result<(), String> {
        match ntfs::open_volume(vol) {
            Ok(h) => {
                unsafe { windows_sys::Win32::Foundation::CloseHandle(h) };
                Ok(())
            }
            Err(e) => Err(e.message.clone()),
        }
    }

    fn working_set_bytes() -> u64 {
        let mut pmc: PROCESS_MEMORY_COUNTERS = unsafe { std::mem::zeroed() };
        pmc.cb = std::mem::size_of::<PROCESS_MEMORY_COUNTERS>() as u32;
        let ok = unsafe {
            K32GetProcessMemoryInfo(
                GetCurrentProcess(),
                &mut pmc,
                std::mem::size_of::<PROCESS_MEMORY_COUNTERS>() as u32,
            )
        };
        if ok == 0 {
            0
        } else {
            pmc.WorkingSetSize as u64
        }
    }

    fn ntfs_volume() -> Option<String> {
        // Prefer C:, else the first fixed NTFS volume.
        for letter in ['C', 'D', 'E', 'F', 'G', 'H'] {
            let root = format!("{letter}:\\");
            if let Ok(fs) = ntfs::filesystem_name(&root) {
                if fs.eq_ignore_ascii_case("NTFS") {
                    return Some(root);
                }
            }
        }
        None
    }

    fn scan_volume_for_test(vol: &str) -> DiskSnapshot {
        let plan = plan_for_path(vol).expect("plan resolves");
        assert_eq!(
            plan.kind,
            ScannerKind::NtfsMft,
            "expected the NTFS fast path"
        );
        let cancel = AtomicBool::new(false);
        match scan_volume(&plan, &cancel, None) {
            Ok((snap, _)) => snap,
            Err(e) => {
                live_report(
                    "FAIL",
                    &format!("NTFS MFT scan attempted on {vol} but failed unexpectedly: {e}"),
                );
                panic!("NTFS fast path attempted and failed unexpectedly: {e}");
            }
        }
    }

    #[test]
    fn live_ntfs_scan_is_fast_and_correct() {
        if !live_enabled() {
            live_skip("live diagnostic harness not enabled; run with WINKIT_LIVE_WINDOWS=1");
            return;
        }
        let vol = match ntfs_volume() {
            Some(v) => v,
            None => {
                live_skip("no NTFS volume found");
                return;
            }
        };
        if let Err(reason) = fast_path_probe(&vol) {
            live_skip(&format!(
                "NTFS fast path NOT run: volume access unavailable ({reason}); a skipped fast-path test is not proof of performance — run the MCP with administrator rights to exercise the MFT scan"
            ));
            return;
        }
        let t0 = Instant::now();
        let snap = scan_volume_for_test(&vol);
        let elapsed_ms = t0.elapsed().as_millis() as u64;
        eprintln!(
            "[bench] {vol} scan: records={} files={} dirs={} enum={}ms size={}ms index={}ms total={}ms",
            snap.records.len(),
            snap.counts.files,
            snap.counts.dirs,
            snap.timings.enum_ms,
            snap.timings.size_ms,
            snap.timings.index_ms,
            elapsed_ms
        );
        assert!(snap.counts.files > 0, "expected files on {vol}");
        assert!(snap.counts.dirs > 0, "expected directories on {vol}");
        // Root aggregate equals the sum of all files.
        let root = snap.root_index().unwrap();
        assert_eq!(
            snap.tree.aggregate[root as usize],
            snap.counts.total_logical
        );
        // Top files are sorted descending and resolve back to themselves.
        let top = snap.top_files(None, 5, 0, None);
        assert!(!top.is_empty());
        for w in top.windows(2) {
            assert!(w[0].size_bytes >= w[1].size_bytes);
        }
        for f in &top {
            let idx = snap.resolve_path(&f.path).expect("path resolves");
            assert_eq!(snap.path_of(idx), f.path, "path round-trip");
        }
        // Re-query from the precomputed lists is effectively free.
        let t1 = Instant::now();
        let _ = snap.top_files(None, 10, 0, None);
        let _ = snap.top_folders(None, 10);
        let cached_ms = t1.elapsed().as_millis() as u64;
        live_report(
            "PASS",
            &format!(
                "real NTFS MFT scan executed on {vol}: files={} dirs={} enum_ms={} size_ms={} index_ms={} total_ms={} cached_query_ms={}",
                snap.counts.files,
                snap.counts.dirs,
                snap.timings.enum_ms,
                snap.timings.size_ms,
                snap.timings.index_ms,
                elapsed_ms,
                cached_ms
            ),
        );
    }

    /// Environment diagnostic (run explicitly: `cargo test --features live-windows -- --ignored probe_environment`).
    #[ignore]
    #[test]
    fn probe_environment() {
        if !live_enabled() {
            live_skip("live diagnostic harness not enabled");
            return;
        }
        use windows_sys::Win32::Storage::FileSystem::GetLogicalDrives;
        let mask = unsafe { GetLogicalDrives() };
        eprintln!("[probe] drives mask = {mask:#010x}");
        for letter in 'A'..='Z' {
            if mask & (1 << (letter as u8 - b'A')) == 0 {
                continue;
            }
            let root = format!("{letter}:\\");
            eprintln!("[probe] drive {root}");
            match ntfs::filesystem_name(&root) {
                Ok(fs) => eprintln!("[probe]   filesystem = {fs}"),
                Err(e) => eprintln!("[probe]   filesystem error = {e}"),
            }
            let dev: String = ['\\', '\\', '.', '\\', letter, ':'].iter().collect();
            eprintln!("[probe]   opening {dev} with GENERIC_READ ...");
            let t = Instant::now();
            let r = ntfs::open_volume(&root);
            eprintln!(
                "[probe]   open took {} ms: {:?}",
                t.elapsed().as_millis(),
                r.as_ref().err()
            );
            // Try alternate access modes.
            for (label, access) in [
                (
                    "FILE_READ_ATTRIBUTES",
                    windows_sys::Win32::Storage::FileSystem::FILE_READ_ATTRIBUTES,
                ),
                (
                    "GENERIC_READ|FILE_READ_ATTRIBUTES",
                    windows_sys::Win32::Foundation::GENERIC_READ
                        | windows_sys::Win32::Storage::FileSystem::FILE_READ_ATTRIBUTES,
                ),
            ] {
                let wide = crate::utils::to_wide(&dev);
                let h = unsafe {
                    windows_sys::Win32::Storage::FileSystem::CreateFileW(
                        wide.as_ptr(),
                        access,
                        windows_sys::Win32::Storage::FileSystem::FILE_SHARE_READ
                            | windows_sys::Win32::Storage::FileSystem::FILE_SHARE_WRITE
                            | windows_sys::Win32::Storage::FileSystem::FILE_SHARE_DELETE,
                        std::ptr::null(),
                        windows_sys::Win32::Storage::FileSystem::OPEN_EXISTING,
                        windows_sys::Win32::Storage::FileSystem::FILE_ATTRIBUTE_NORMAL,
                        std::ptr::null_mut(),
                    )
                };
                if h == windows_sys::Win32::Foundation::INVALID_HANDLE_VALUE {
                    eprintln!("[probe]   {label}: denied ({})", unsafe {
                        windows_sys::Win32::Foundation::GetLastError()
                    });
                    continue;
                }
                eprintln!("[probe]   {label}: OPENED");
                // Try FSCTL_ENUM_USN_DATA through this handle.
                let input = windows_sys::Win32::System::Ioctl::MFT_ENUM_DATA_V0 {
                    StartFileReferenceNumber: 0,
                    LowUsn: 0,
                    HighUsn: i64::MAX,
                };
                let mut out = vec![0u8; 1024 * 1024];
                let mut returned: u32 = 0;
                let ok = unsafe {
                    windows_sys::Win32::System::IO::DeviceIoControl(
                        h,
                        windows_sys::Win32::System::Ioctl::FSCTL_ENUM_USN_DATA,
                        &input as *const _ as *const std::ffi::c_void,
                        std::mem::size_of::<windows_sys::Win32::System::Ioctl::MFT_ENUM_DATA_V0>()
                            as u32,
                        out.as_mut_ptr() as *mut std::ffi::c_void,
                        out.len() as u32,
                        &mut returned,
                        std::ptr::null_mut(),
                    )
                };
                eprintln!(
                    "[probe]   FSCTL_ENUM_USN_DATA: ok={ok} returned={returned} err={}",
                    unsafe { windows_sys::Win32::Foundation::GetLastError() }
                );
                if ok != 0 && returned > 0 {
                    let nxt = u64::from_le_bytes(out[0..8].try_into().unwrap());
                    eprintln!(
                        "[probe]   next-start FRN = {nxt}; first record major version = {}",
                        u16::from_le_bytes([out[12], out[13]])
                    );
                }
                unsafe { windows_sys::Win32::Foundation::CloseHandle(h) };
            }
            if let Ok(h) = r {
                unsafe { windows_sys::Win32::Foundation::CloseHandle(h) };
            }
            eprintln!("[probe]   root FRN ...");
            let t = Instant::now();
            eprintln!(
                "[probe]   root_frn took {} ms: {:?}",
                t.elapsed().as_millis(),
                ntfs::root_file_reference(&root)
            );
        }
        let vol = std::env::var("SystemDrive").unwrap_or_else(|_| "C:".into());
        eprintln!("[probe] SystemDrive = {vol}");
    }

    #[test]
    fn live_scan_reports_memory_and_timings() {
        if !live_enabled() {
            live_skip("live diagnostic harness not enabled; run with WINKIT_LIVE_WINDOWS=1");
            return;
        }
        let vol = match ntfs_volume() {
            Some(v) => v,
            None => {
                live_skip("no NTFS volume found");
                return;
            }
        };
        if let Err(reason) = fast_path_probe(&vol) {
            live_skip(&format!(
                "memory report needs a real NTFS scan; volume access unavailable ({reason})"
            ));
            return;
        }
        let before = working_set_bytes();
        let snap = scan_volume_for_test(&vol);
        let after = working_set_bytes();
        let approx_records_mb = snap.records.len() as u64 * 48 / (1024 * 1024);
        let approx_names_mb = snap.names.len() as u64 / (1024 * 1024);
        let ws_delta_mb = (after.saturating_sub(before)) / (1024 * 1024);
        eprintln!(
            "[bench] {vol}: entries={} files={} dirs={} enum_ms={} size_ms={} index_ms={} total_ms={} peak_ws_delta_mb={} approx_records_mb={} approx_names_mb={}",
            snap.records.len(),
            snap.counts.files,
            snap.counts.dirs,
            snap.timings.enum_ms,
            snap.timings.size_ms,
            snap.timings.index_ms,
            snap.timings.total_ms,
            ws_delta_mb,
            approx_records_mb,
            approx_names_mb
        );
        live_report(
            "PASS",
            &format!(
                "real NTFS MFT scan on {vol}: peak working-set delta {ws_delta_mb} MB, approx records {approx_records_mb} MB, names {approx_names_mb} MB"
            ),
        );
    }

    #[test]
    fn live_precancelled_scan_aborts() {
        if !live_enabled() {
            live_skip("live diagnostic harness not enabled; run with WINKIT_LIVE_WINDOWS=1");
            return;
        }
        let vol = match ntfs_volume() {
            Some(v) => v,
            None => {
                live_skip("no NTFS volume found");
                return;
            }
        };
        if let Err(reason) = fast_path_probe(&vol) {
            live_skip(&format!(
                "cancellation path exercised by unit tests; volume access unavailable ({reason})"
            ));
            return;
        }
        let plan = plan_for_path(&vol).unwrap();
        let cancel = AtomicBool::new(true);
        match scan_volume(&plan, &cancel, None) {
            Err(e) if e.kind == ErrorKind::Cancelled => live_report(
                "PASS",
                &format!("pre-cancelled NTFS scan aborted with Cancelled on {vol}"),
            ),
            Err(e) => {
                live_report("FAIL", &format!("pre-cancelled scan did not abort: {e}"));
                panic!("pre-cancelled scan must abort, got: {e}");
            }
            Ok(_) => {
                live_report("FAIL", "pre-cancelled scan completed instead of aborting");
                panic!("pre-cancelled scan must abort");
            }
        }
    }

    /// The fallback scanner over a bounded real directory, exercising the
    /// full snapshot/query machinery on real files in any environment.
    #[test]
    fn live_fallback_scan_on_real_directory() {
        if !live_enabled() {
            live_skip("live diagnostic harness not enabled; run with WINKIT_LIVE_WINDOWS=1");
            return;
        }
        // Scan the project source tree (bounded, real, always present).
        let dir = std::env::current_dir().expect("cwd");
        let plan = ScannerPlan {
            volume_root: dir.to_string_lossy().into_owned(),
            scope_root: dir.to_string_lossy().into_owned(),
            filesystem: "any".into(),
            kind: ScannerKind::RecursiveFallback,
        };
        let cancel = AtomicBool::new(false);
        let (snap, _) = scan_volume(&plan, &cancel, None).expect("fallback scan");
        assert!(
            snap.counts.files > 0,
            "expected files under {}",
            dir.display()
        );
        assert!(snap.counts.dirs > 0);
        let root = snap.root_index().unwrap();
        assert_eq!(
            snap.tree.aggregate[root as usize],
            snap.counts.total_logical
        );
        let top = snap.top_files(None, 5, 0, None);
        assert!(!top.is_empty());
        for w in top.windows(2) {
            assert!(w[0].size_bytes >= w[1].size_bytes);
        }
        for f in &top {
            let idx = snap.resolve_path(&f.path).expect("path resolves");
            assert_eq!(snap.path_of(idx), f.path);
        }
        // A subtree query under a real directory.
        let subdir = snap
            .resolve_path(&format!("{}\\src", dir.to_string_lossy()))
            .expect("src resolves");
        let sz = snap.folder_size(subdir);
        assert!(sz.size_bytes > 0);
        eprintln!(
            "[bench] fallback scan of {}: files={} dirs={} total_ms={} top_file={} bytes",
            dir.display(),
            snap.counts.files,
            snap.counts.dirs,
            snap.timings.total_ms,
            top[0].size_bytes
        );
        live_report(
            "PASS",
            &format!(
                "fallback scan of a real directory {}: files={} dirs={} total_ms={}",
                dir.display(),
                snap.counts.files,
                snap.counts.dirs,
                snap.timings.total_ms
            ),
        );
    }

    /// Real MFT-vs-fallback benchmark on the same path. When the fast path
    /// cannot run (volume access denied), the benchmark reports the exact
    /// reason and is left BLOCKED — it never pretends timings exist.
    #[test]
    fn live_benchmark_compare_mft_vs_fallback() {
        if !live_enabled() {
            live_skip("live diagnostic harness not enabled; run with WINKIT_LIVE_WINDOWS=1");
            return;
        }
        let vol = match ntfs_volume() {
            Some(v) => v,
            None => {
                live_report(
                    "BLOCKED",
                    "no NTFS volume found; benchmark needs a real NTFS volume",
                );
                return;
            }
        };
        if let Err(reason) = fast_path_probe(&vol) {
            live_report(
                "BLOCKED",
                &format!(
                    "cannot open {vol} with GENERIC_READ ({reason}); the recursive-vs-MFT benchmark cannot run without fast-path access — no MFT timings to compare. The fallback path is exercised by unit tests and the MCP release-binary tests instead."
                ),
            );
            return;
        }

        let cancel = AtomicBool::new(false);
        // Leg 1: NTFS MFT scanner plus size pass.
        let plan = plan_for_path(&vol).expect("plan resolves");
        let ws_before = working_set_bytes();
        let t0 = Instant::now();
        let (mft, _) = scan_volume(&plan, &cancel, None).expect("MFT scan");
        let mft_total_ms = t0.elapsed().as_millis() as u64;
        let mft_ws_delta = working_set_bytes().saturating_sub(ws_before);
        let tq = Instant::now();
        let _ = mft.top_files(None, 10, 0, None);
        let cached_ms = tq.elapsed().as_millis() as u64;

        // Leg 2: recursive fallback over the same path.
        let fplan = ScannerPlan {
            volume_root: plan.volume_root.clone(),
            scope_root: plan.volume_root.clone(),
            filesystem: plan.filesystem.clone(),
            kind: ScannerKind::RecursiveFallback,
        };
        let fws_before = working_set_bytes();
        let tf = Instant::now();
        let fallback = scan_volume(&fplan, &cancel, None);
        let fb_total_ms = tf.elapsed().as_millis() as u64;
        let fb_ws_delta = working_set_bytes().saturating_sub(fws_before);

        eprintln!("[bench] filesystem=NTFS volume={vol}");
        eprintln!(
            "[bench] MFT scanner      : files={} dirs={} logical_bytes={} enum_ms={} size_ms={} index_ms={} total_ms={} cached_query_ms={} peak_ws_delta_mb={} scanner=ntfs_mft_fast",
            mft.counts.files,
            mft.counts.dirs,
            mft.counts.total_logical,
            mft.timings.enum_ms,
            mft.timings.size_ms,
            mft.timings.index_ms,
            mft_total_ms,
            cached_ms,
            mft_ws_delta / (1024 * 1024)
        );
        match &fallback {
            Ok((fb, _)) => {
                eprintln!(
                    "[bench] fallback scanner : files={} dirs={} logical_bytes={} walk_ms={} index_ms={} total_ms={} peak_ws_delta_mb={} scanner=recursive_fallback",
                    fb.counts.files,
                    fb.counts.dirs,
                    fb.counts.total_logical,
                    fb.timings.enum_ms,
                    fb.timings.index_ms,
                    fb_total_ms,
                    fb_ws_delta / (1024 * 1024)
                );
                live_report(
                    "PASS",
                    &format!(
                        "benchmark ran on {vol}: MFT {mft_total_ms} ms vs recursive fallback {fb_total_ms} ms (files {}/{}), see [bench] lines",
                        mft.counts.files,
                        fb.counts.files
                    ),
                );
            }
            Err(e) => {
                eprintln!("[bench] fallback scanner : BLOCKED ({e})");
                live_report(
                    "PASS",
                    &format!(
                        "benchmark ran MFT leg only on {vol} ({mft_total_ms} ms); recursive fallback leg blocked: {e}"
                    ),
                );
            }
        }
    }

    #[test]
    fn live_service_caches_snapshot() {
        if !live_enabled() {
            live_skip("live diagnostic harness not enabled; run with WINKIT_LIVE_WINDOWS=1");
            return;
        }
        let vol = match ntfs_volume() {
            Some(v) => v,
            None => {
                live_skip("no NTFS volume found");
                return;
            }
        };
        if let Err(reason) = fast_path_probe(&vol) {
            live_skip(&format!(
                "cache behavior covered by unit tests; volume access unavailable ({reason})"
            ));
            return;
        }
        let service = DiskScanService::default();
        let req = DiskScanRequest {
            path: vol.clone(),
            refresh: false,
            max_age_ms: 0,
        };
        let first = service.sync_scan(&req).expect("first scan");
        assert!(!first.cached);
        let second = service.sync_scan(&req).expect("cached scan");
        assert!(second.cached, "second call must be served from cache");
        assert_eq!(second.files_indexed, first.files_indexed);
        // A forced refresh rescans.
        let req = DiskScanRequest {
            path: vol.clone(),
            refresh: true,
            max_age_ms: 0,
        };
        let third = service.sync_scan(&req).expect("refreshed scan");
        assert!(!third.cached);
        live_report(
            "PASS",
            &format!(
                "service cache on {vol}: fresh scan then cached then forced refresh (cached flags {}/{}/{})",
                first.cached, second.cached, third.cached
            ),
        );
    }
}
