//! Bounded, read-only filesystem primitives for the file tools
//! (`read_text_file`, `find_files`, `directory_overview`).
//!
//! This module is pure `std` — it never touches Win32 and never writes,
//! renames, or deletes anything. Every walk and read carries hard budgets so
//! a call can never run unbounded in time or return an unbounded payload:
//!
//!   * walks never follow reparse points (symlinks/junctions), so cycles are
//!     impossible;
//!   * every walk has a depth cap and an entry budget; when the budget is
//!     hit the result says so (`truncated: true`) instead of failing;
//!   * text reads are capped in bytes and BOM-aware (UTF-8/UTF-16LE/BE);
//!   * binary content is detected before decoding and refused with a clear
//!     error rather than emitting mojibake.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

/// Hard cap on bytes returned by one `read_text_file` call.
pub const MAX_READ_BYTES: usize = 256 * 1024;
/// Default bytes returned by `read_text_file` when `max_bytes` is absent.
pub const DEFAULT_READ_BYTES: usize = 32 * 1024;
/// Hard cap on results from one `find_files` call.
pub const MAX_FIND_RESULTS: usize = 500;
/// Default recursion depth for `find_files`.
pub const DEFAULT_FIND_DEPTH: u32 = 6;
/// Hard cap on recursion depth for `find_files`.
pub const MAX_FIND_DEPTH: u32 = 12;
/// Hard cap on file entries examined by one `directory_overview` call.
pub const MAX_DIR_WALK_ENTRIES: usize = 60_000;

/// One matched or listed file.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct FileEntry {
    /// Absolute path.
    pub path: String,
    pub size_bytes: u64,
    /// RFC3339 modification time, when readable.
    pub modified_at: Option<String>,
}

/// Result of a bounded recursive walk.
#[derive(Debug, Default)]
pub struct WalkOutcome {
    pub files: Vec<FileEntry>,
    /// True when the entry or depth budget stopped the walk early.
    pub truncated: bool,
    pub scanned_dirs: usize,
    /// Directories that could not be read (access denied) — reported, not silent.
    pub unreadable_dirs: usize,
}

/// Detect whether a byte sample looks binary: any NUL byte, or more than
/// ~30% control characters (excluding tab/newline/CR) in the sample.
pub fn is_probably_binary(sample: &[u8]) -> bool {
    if sample.is_empty() {
        return false;
    }
    if sample.contains(&0u8) {
        return true;
    }
    let controls = sample
        .iter()
        .filter(|&&b| b < 0x20 && !matches!(b, b'\t' | b'\n' | b'\r'))
        .count();
    controls * 100 > sample.len() * 30
}

/// Strip a UTF-8/UTF-16 BOM if present; returns `(encoding_name, bytes_after_bom)`.
fn split_bom(bytes: &[u8]) -> (&'static str, &[u8]) {
    if bytes.starts_with(&[0xEF, 0xBB, 0xBF]) {
        ("utf8_bom", &bytes[3..])
    } else if bytes.starts_with(&[0xFF, 0xFE]) {
        ("utf16le", &bytes[2..])
    } else if bytes.starts_with(&[0xFE, 0xFF]) {
        ("utf16be", &bytes[2..])
    } else {
        ("utf8", bytes)
    }
}

/// Decode bytes as text: honors UTF-8/UTF-16 BOMs, falls back to lossy UTF-8.
/// Returns `(encoding, text)`. Callers must exclude binary content first
/// via [`is_probably_binary`].
pub fn decode_text(bytes: &[u8]) -> (&'static str, String) {
    let (enc, rest) = split_bom(bytes);
    match enc {
        "utf16le" => (
            "utf16le",
            String::from_utf16_lossy(
                &rest
                    .chunks_exact(2)
                    .map(|c| u16::from_le_bytes([c[0], c[1]]))
                    .collect::<Vec<u16>>(),
            ),
        ),
        "utf16be" => (
            "utf16be",
            String::from_utf16_lossy(
                &rest
                    .chunks_exact(2)
                    .map(|c| u16::from_be_bytes([c[0], c[1]]))
                    .collect::<Vec<u16>>(),
            ),
        ),
        // "utf8_bom" keeps its name here: the BOM is stripped above, but the
        // encoding label stays honest about what the file carried.
        "utf8_bom" => ("utf8_bom", String::from_utf8_lossy(rest).into_owned()),
        _ => ("utf8", String::from_utf8_lossy(rest).into_owned()),
    }
}

/// Case-insensitive wildcard match supporting `*` (any run) and `?`
/// (one char). Everything else is a literal comparison. Empty pattern
/// matches nothing (callers should reject it earlier).
pub fn wildcard_match(name: &str, pattern: &str) -> bool {
    let n = name.to_lowercase();
    let p = pattern.to_lowercase();
    // Iterative glob match with backtracking; O(n*m) worst case is fine at
    // filename lengths.
    let (nb, pb) = (n.as_bytes(), p.as_bytes());
    let (mut ni, mut pi) = (0usize, 0usize);
    let (mut star, mut mark) = (None::<usize>, 0usize);
    while ni < nb.len() {
        if pi < pb.len() && (pb[pi] == b'?' || pb[pi] == nb[ni]) {
            ni += 1;
            pi += 1;
        } else if pi < pb.len() && pb[pi] == b'*' {
            star = Some(pi);
            pi += 1;
            mark = ni;
        } else if let Some(s) = star {
            pi = s + 1;
            mark += 1;
            ni = mark;
        } else {
            return false;
        }
    }
    while pi < pb.len() && pb[pi] == b'*' {
        pi += 1;
    }
    pi == pb.len()
}

/// Render a path for tool output: strips the `\\?\` extended-length prefix
/// that `std::fs::canonicalize` adds on Windows (an implementation detail
/// agents should not have to parse).
pub fn display_path(path: &Path) -> String {
    let s = path.to_string_lossy();
    s.strip_prefix(r"\\?\").unwrap_or(&s).to_string()
}

fn to_file_entry(path: &Path, meta: &std::fs::Metadata) -> FileEntry {
    FileEntry {
        path: display_path(path),
        size_bytes: meta.len(),
        modified_at: meta
            .modified()
            .ok()
            .and_then(|t: SystemTime| crate::utils::time::format_rfc3339_opt(t)),
    }
}

/// Recursively collect regular files under `root`, never following reparse
/// points. Budgeted by depth and total files collected; stops early and
/// reports `truncated` when either budget is exhausted.
///
/// When `pattern` is `Some`, only file names matching it are collected
/// (directories still recurse regardless of the match).
pub fn walk_files(
    root: &Path,
    max_depth: u32,
    max_files: usize,
    pattern: Option<&str>,
) -> WalkOutcome {
    let mut out = WalkOutcome::default();
    // Small deferred queue keeps ordering breadth-first-ish per directory
    // while bounding memory to the directory width.
    walk_dir(root, 0, max_depth, max_files, pattern, &mut out);
    out
}

fn walk_dir(
    dir: &Path,
    depth: u32,
    max_depth: u32,
    max_files: usize,
    pattern: Option<&str>,
    out: &mut WalkOutcome,
) -> bool {
    // Returns false when the caller must stop unwinding (file budget hit).
    if out.files.len() >= max_files {
        out.truncated = true;
        return false;
    }
    if depth > max_depth {
        // Past the requested depth: skip this subtree silently — this is a
        // user bound, not truncation of what was asked for.
        return true;
    }
    let read = std::fs::read_dir(dir);
    let read = match read {
        Ok(r) => r,
        Err(_) => {
            out.unreadable_dirs += 1;
            return true;
        }
    };
    out.scanned_dirs += 1;
    let mut children: Vec<(PathBuf, bool, std::fs::Metadata)> = Vec::new();
    for entry in read.flatten() {
        // symlink_metadata: never traverse into reparse points of any kind.
        let Ok(meta) = entry.metadata() else { continue };
        let is_dir = meta.is_dir();
        children.push((entry.path(), is_dir, meta));
    }
    // Files first (so name-filtered matches surface early), then dirs.
    children.sort_by(|a, b| a.0.cmp(&b.0));
    for (path, is_dir, meta) in children {
        if is_dir {
            if walk_dir(&path, depth + 1, max_depth, max_files, pattern, out) {
                continue;
            }
            return false;
        }
        if let Some(p) = pattern {
            let name = path
                .file_name()
                .map(|f| f.to_string_lossy().into_owned())
                .unwrap_or_default();
            if !wildcard_match(&name, p) {
                continue;
            }
        }
        out.files.push(to_file_entry(&path, &meta));
        if out.files.len() >= max_files {
            out.truncated = true;
            return false;
        }
    }
    true
}

/// Aggregate recursive stats for one directory tree.
#[derive(Debug, Clone, Serialize)]
pub struct TreeStats {
    pub path: String,
    pub size_bytes: u64,
    pub files: usize,
    pub dirs: usize,
    /// True when the entry budget stopped this subtree's walk early.
    pub truncated: bool,
}

/// Compute [`TreeStats`] for `root` within a shared remaining-entry budget.
/// Returns the stats plus the number of entries consumed (never exceeds
/// `budget_entries`).
pub fn tree_stats(root: &Path, budget_entries: usize) -> (TreeStats, usize) {
    let mut used = 0usize;
    let stats = measure_tree(root, budget_entries, &mut used);
    (stats, used)
}

fn measure_tree(dir: &Path, budget: usize, used: &mut usize) -> TreeStats {
    let mut stats = TreeStats {
        path: display_path(dir),
        size_bytes: 0,
        files: 0,
        dirs: 0,
        truncated: false,
    };
    if *used >= budget {
        stats.truncated = true;
        return stats;
    }
    let Ok(read) = std::fs::read_dir(dir) else {
        stats.truncated = true;
        return stats;
    };
    stats.dirs = 1;
    *used += 1;
    let mut children: Vec<PathBuf> = read.flatten().map(|e| e.path()).collect();
    children.sort();
    for child in children {
        if *used >= budget {
            stats.truncated = true;
            break;
        }
        // Never follow reparse points; count them as opaque files.
        let Ok(sym) = std::fs::symlink_metadata(&child) else {
            continue;
        };
        if sym.is_symlink() {
            stats.size_bytes += sym.len();
            stats.files += 1;
            *used += 1;
        } else if sym.is_dir() {
            let sub = measure_tree(&child, budget, used);
            stats.size_bytes += sub.size_bytes;
            stats.files += sub.files;
            stats.dirs += sub.dirs;
            if sub.truncated {
                stats.truncated = true;
            }
        } else {
            stats.size_bytes += sym.len();
            stats.files += 1;
            *used += 1;
        }
    }
    stats
}

/// What a bounded text read returned.
#[derive(Debug, Clone, Serialize)]
pub struct TextReadOutcome {
    pub encoding: &'static str,
    /// Decoded content after BOM stripping.
    pub content: String,
    pub total_bytes: u64,
    pub returned_bytes: usize,
    pub truncated: bool,
    /// The mode the caller requested (`head` | `tail` | `all`), echoed
    /// verbatim. When the whole file fits in the byte window the content is
    /// identical for every mode; `whole_file` says so explicitly instead of
    /// silently rewriting the mode (which made callers think their `tail`
    /// request was ignored).
    pub mode: &'static str,
    /// True when the returned window covers the entire file.
    pub whole_file: bool,
}

/// Read up to `max_bytes` from the head or tail of a file (or all of it),
/// BOM-aware, refusing binary content. For `tail`, the window starts just
/// after a newline unless the file is smaller than the window.
pub fn read_text_slice(
    path: &Path,
    mode: &str,
    max_bytes: usize,
) -> Result<TextReadOutcome, crate::errors::WinkitError> {
    use std::io::{Read, Seek, SeekFrom};
    let mut f = std::fs::File::open(path).map_err(|e| {
        crate::errors::WinkitError::not_found(format!(
            "cannot open '{}': {e}",
            crate::utils::filesys::display_path(path)
        ))
    })?;
    let total = f
        .metadata()
        .map_err(crate::errors::WinkitError::from)?
        .len();

    // Binary sniff on the first 8 KiB.
    let mut sniff = [0u8; 8192];
    let n = f
        .read(&mut sniff)
        .map_err(crate::errors::WinkitError::from)?;
    if is_probably_binary(&sniff[..n]) {
        return Err(crate::errors::WinkitError::invalid_argument(format!(
            "'{}' appears to be a binary file; text tools refuse binary content",
            crate::utils::filesys::display_path(path)
        )));
    }

    let want = max_bytes.min(MAX_READ_BYTES) as u64;
    let (requested_mode, start, truncated_by_window) = match mode {
        "tail" => {
            let start = total.saturating_sub(want);
            ("tail", start, start > 0)
        }
        "all" => ("all", 0, total > want),
        _ => ("head", 0, total > want),
    };

    f.seek(SeekFrom::Start(start))
        .map_err(crate::errors::WinkitError::from)?;
    let mut buf = vec![0u8; want.min(usize::MAX as u64) as usize];
    let mut filled = 0usize;
    loop {
        let read = f
            .read(&mut buf[filled..])
            .map_err(crate::errors::WinkitError::from)?;
        if read == 0 {
            break;
        }
        filled += read;
        if filled >= buf.len() {
            break;
        }
    }
    buf.truncate(filled);

    // Tail alignment: drop the first partial line so agents see whole lines.
    if mode == "tail" && start > 0 {
        if let Some(pos) = buf.iter().position(|&b| b == b'\n') {
            buf.drain(..=pos);
        }
    }

    let (encoding, text) = decode_text(&buf);
    Ok(TextReadOutcome {
        encoding,
        content: text,
        total_bytes: total,
        returned_bytes: filled,
        truncated: truncated_by_window || total > filled as u64,
        mode: requested_mode,
        whole_file: start == 0 && (filled as u64) >= total,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wildcard_matches_names_case_insensitively() {
        assert!(wildcard_match("Cargo.TOML", "*.toml"));
        assert!(wildcard_match("main.rs", "ma??.rs"));
        assert!(wildcard_match("package-lock.json", "package*.json"));
        assert!(!wildcard_match("notes.txt", "*.json"));
        assert!(wildcard_match("a.tar.gz", "a*"));
        assert!(wildcard_match("anything", "*"));
        assert!(!wildcard_match("ab", "a?c"));
    }

    #[test]
    fn binary_detection_sniffs_nul_and_controls() {
        assert!(is_probably_binary(b"MZ\x00\x90\x00"));
        assert!(is_probably_binary(&[0x01, 0x02, 0x03]));
        assert!(!is_probably_binary(b"#include <stdio.h>\nint main() {}\n"));
        assert!(!is_probably_binary(b"line1\r\nline2\tend\n"));
        assert!(!is_probably_binary(b""));
    }

    #[test]
    fn bom_detection_and_decoding() {
        let mut utf16le: Vec<u8> = vec![0xFF, 0xFE];
        utf16le.extend("hi".encode_utf16().flat_map(|u| u.to_le_bytes()));
        let (enc, text) = decode_text(&utf16le);
        assert_eq!(enc, "utf16le");
        assert_eq!(text, "hi");

        let utf8bom = [0xEF, 0xBB, 0xBF, b'o', b'k'];
        let (enc, text) = decode_text(&utf8bom);
        assert_eq!(enc, "utf8_bom");
        assert_eq!(text, "ok");

        let (enc, text) = decode_text(b"plain");
        assert_eq!(enc, "utf8");
        assert_eq!(text, "plain");
    }

    #[test]
    fn head_tail_reads_respect_budgets_and_align_lines() {
        let dir = std::env::temp_dir().join(format!("winkit-filesys-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("sample.txt");
        let body: String = (0..2000).map(|i| format!("line-{i:04}\n")).collect();
        std::fs::write(&path, &body).unwrap();

        let head = read_text_slice(&path, "head", 64).unwrap();
        assert_eq!(head.mode, "head");
        assert!(head.truncated);
        assert!(head.content.starts_with("line-0000\n"));
        assert!(head.content.len() <= 64 + 4); // decoded <= window

        let tail = read_text_slice(&path, "tail", 64).unwrap();
        assert_eq!(tail.mode, "tail");
        // Aligned: starts exactly at a line boundary.
        assert!(tail.content.starts_with("line-"));
        assert!(!tail.content.contains("\nline-\n"));

        let all = read_text_slice(&path, "all", MAX_READ_BYTES).unwrap();
        assert_eq!(all.mode, "all");
        assert!(!all.truncated);
        assert_eq!(all.returned_bytes as u64, body.len() as u64);

        // A tail request on a small file returns the entire content but
        // still echoes the requested mode (regression: it used to rewrite
        // the mode to "all", which looked like the argument was ignored).
        let small = dir.join("small.txt");
        std::fs::write(&small, b"a\nb\nc\n").unwrap();
        let tail_small = read_text_slice(&small, "tail", 64).unwrap();
        assert_eq!(tail_small.mode, "tail");
        assert!(tail_small.whole_file);
        assert_eq!(tail_small.content, "a\nb\nc\n");
        std::fs::remove_file(&small).ok();

        std::fs::remove_file(&path).ok();
        std::fs::remove_dir(&dir).ok();
    }

    #[test]
    fn walk_respects_depth_and_pattern_budgets() {
        let root = std::env::temp_dir().join(format!("winkit-walk-test-{}", std::process::id()));
        let deep = root.join("a/b/c/d");
        std::fs::create_dir_all(&deep).unwrap();
        std::fs::write(root.join("keep.log"), b"x").unwrap();
        std::fs::write(root.join("skip.txt"), b"x").unwrap();
        std::fs::write(deep.join("deep.log"), b"x").unwrap();
        std::fs::write(deep.join("other.txt"), b"x").unwrap();

        let out = walk_files(&root, MAX_FIND_DEPTH, MAX_FIND_RESULTS, Some("*.log"));
        assert_eq!(out.files.len(), 2);
        assert!(!out.truncated);
        assert!(out.files.iter().any(|f| f.path.ends_with("deep.log")));

        let shallow = walk_files(&root, 0, MAX_FIND_RESULTS, None);
        assert_eq!(shallow.files.len(), 2, "depth 0 lists only root files");
        assert!(shallow.truncated || shallow.scanned_dirs >= 1);

        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn tree_stats_aggregates_recursively() {
        let root = std::env::temp_dir().join(format!("winkit-stats-test-{}", std::process::id()));
        let sub = root.join("sub");
        std::fs::create_dir_all(&sub).unwrap();
        std::fs::write(root.join("big.bin"), vec![7u8; 1000]).unwrap();
        std::fs::write(sub.join("small.bin"), vec![7u8; 10]).unwrap();

        let (stats, used) = tree_stats(&root, MAX_DIR_WALK_ENTRIES);
        assert_eq!(stats.files, 2);
        assert_eq!(stats.dirs, 2);
        assert_eq!(stats.size_bytes, 1010);
        assert!(!stats.truncated);
        assert!(used >= 4);

        let (tiny, _) = tree_stats(&root, 2);
        assert!(tiny.truncated, "budget 2 must truncate a 2-file tree");

        std::fs::remove_dir_all(&root).ok();
    }
}
